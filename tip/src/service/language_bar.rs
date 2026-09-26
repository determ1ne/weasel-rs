//! 实现 TSF 语言栏中的中/英输入模式按钮，并通过 `ITfLangBarItemSink` 通知外壳刷新。
//!
//! 按钮状态由原子字段发布；当前上下文仅以弱引用关联，避免语言栏延长上下文生命周期。
//! COM 回调会经边界保护转换错误；调用外壳或显示模态菜单时不持有服务锁，以容许同步重入。
use super::*;
use crate::bindings::*;
use crate::icons::taskbar_is_light;
use std::sync::Weak;
use weasel_common::message::{ContextAction, ContextCommand};
use windows_core::{ComObject, HRESULT};
use windows_strings::{BSTR, HSTRING, PCWSTR, w};

/// 本按钮唯一支持的 `ITfLangBarItemSink` 订阅标识。
const SINK_COOKIE: u32 = 1;

/// 在鼠标位置显示原生命令菜单，并把选中项分发给对应功能。
///
/// 菜单句柄由局部守卫确保销毁。弹出菜单期间不持有服务锁，因为菜单循环会运行嵌套消息泵，
/// 可能重入 TIP；没有焦点窗口时直接返回，菜单创建、填充或命令投递失败则返回系统错误。
fn show_command_menu(point: &POINT) -> Result<()> {
    struct Menu(HMENU);
    impl Drop for Menu {
        fn drop(&mut self) {
            unsafe {
                let _ = DestroyMenu(self.0);
            }
        }
    }
    let menu = Menu(unsafe { CreatePopupMenu() });
    if menu.0.0.is_null() {
        return Err(Error::from_thread());
    }
    for (id, label) in weasel_common::command_menu::items(weasel_common::about::shift_pressed()) {
        let label = HSTRING::from(label);
        let flags = if id == 0 { MF_SEPARATOR } else { MF_STRING };
        if !unsafe { AppendMenuW(menu.0, flags as u32, id as usize, PCWSTR(label.as_ptr())) }
            .as_bool()
        {
            return Err(Error::from_thread());
        }
    }
    let owner = unsafe { GetFocus() };
    if owner.0.is_null() {
        return Ok(());
    }
    let command = unsafe {
        TrackPopupMenu(
            menu.0,
            (TPM_NONOTIFY | TPM_RETURNCMD | TPM_RIGHTBUTTON) as u32,
            point.x,
            point.y,
            Some(0),
            owner,
            None,
        )
    };
    if command.0 != 0 {
        dispatch_menu_command(command.0 as u32)?;
    }
    Ok(())
}

/// 校验并分发语言栏菜单命令。
///
/// 关于与诊断对话框在本进程处理；其余已知命令异步投递给现有 broker 窗口。broker 不存在时
/// 安静返回，避免在 TSF 回调中启动进程或等待；未知命令以 `E_INVALIDARG` 拒绝。
fn dispatch_menu_command(command: u32) -> Result<()> {
    if matches!(
        command,
        weasel_common::command_menu::ABOUT | weasel_common::command_menu::DIAGNOSTICS
    ) {
        crate::diagnostics::show_dialog(command == weasel_common::command_menu::DIAGNOSTICS);
        return Ok(());
    }
    if !weasel_common::command_menu::is_command(command) {
        return Err(Error::from_hresult(E_INVALIDARG));
    }
    let class = HSTRING::from(weasel_common::command_menu::BROKER_WINDOW_CLASS);
    let broker = unsafe { FindWindowW(PCWSTR(class.as_ptr()), None) };
    // Broker absence must not start processes or block the host.
    if broker.0.is_null() {
        return Ok(());
    }
    if !unsafe {
        PostMessageW(
            Some(broker),
            WM_COMMAND as u32,
            WPARAM(command as usize),
            LPARAM(0),
        )
    }
    .as_bool()
    {
        return Err(Error::from_thread());
    }
    Ok(())
}

pub(super) struct LanguageBar {
    /// 在整个语言栏项目注册期间保持 TSF 管理器存活。
    manager: ITfLangBarItemMgr,
    /// COM 按钮对象；管理器持有的接口引用可能使其晚于此包装器释放。
    button: ComObject<ModeButton>,
}

impl LanguageBar {
    /// 创建按钮并向 TSF 注册；转换或注册失败时不返回半初始化对象。
    pub fn attach(thread: &ITfThreadMgr) -> Result<Self> {
        // Retain the manager for the complete registration lifetime (Mozc).
        let manager: ITfLangBarItemMgr = thread.cast()?;
        let button = ComObject::new(ModeButton {
            target: Mutex::new(None),
            sink: Mutex::new(None),
            ascii: AtomicBool::new(true),
            mode_known: AtomicBool::new(false),
            connected: AtomicBool::new(true),
            available: AtomicBool::new(false),
            suspended: AtomicBool::new(false),
            secure_field: AtomicBool::new(false),
            allow_rime_in_secure_fields: AtomicBool::new(false),
            light_background: AtomicBool::new(taskbar_is_light()),
            _module: ModuleLease::new(),
        });
        let item: ITfLangBarItemButton = button.to_interface();
        unsafe { manager.AddItem(&item).ok()? };
        Ok(Self { manager, button })
    }

    /// 发布焦点上下文的展示状态，并仅在状态变化时请求外壳刷新。
    ///
    /// `ascii == None` 表示当前连接纪元尚无可信输入模式，不等同于英文模式或连接失败。
    /// 上下文只保存为弱引用。锁竞争会快速返回错误；调用 `OnUpdate` 前释放所有内部锁，
    /// 因为 TSF 可在该调用内同步重入图标、文本和状态查询。
    pub fn update(
        &self,
        context: Option<&Arc<ContextState>>,
        ascii: Option<bool>,
        connection_failed: bool,
        suspended: bool,
        secure_field: bool,
        allow_rime_in_secure_fields: bool,
    ) -> Result<()> {
        let suspended_changed =
            self.button.suspended.swap(suspended, Ordering::AcqRel) != suspended;
        let secure_changed = self
            .button
            .secure_field
            .swap(secure_field, Ordering::AcqRel)
            != secure_field;
        let secure_policy_changed = self
            .button
            .allow_rime_in_secure_fields
            .swap(allow_rime_in_secure_fields, Ordering::AcqRel)
            != allow_rime_in_secure_fields;
        {
            let mut target = self
                .button
                .target
                .try_lock()
                .map_err(|_| Error::from_hresult(E_FAIL))?;
            *target = context.map(Arc::downgrade);
        }
        let available = context.is_some() && !suspended;
        // Unknown is neither English nor a connection failure. Only the
        // focused context's current epoch may supply a known input mode.
        let connected = !connection_failed && !suspended;
        let connection_changed =
            self.button.connected.swap(connected, Ordering::AcqRel) != connected;
        let known = ascii.is_some();
        let known_changed = self.button.mode_known.swap(known, Ordering::AcqRel) != known;
        let mode_changed =
            ascii.is_some_and(|ascii| self.button.ascii.swap(ascii, Ordering::AcqRel) != ascii);
        let availability_changed =
            self.button.available.swap(available, Ordering::AcqRel) != available;
        let light_background = taskbar_is_light();
        let theme_changed = self
            .button
            .light_background
            .swap(light_background, Ordering::AcqRel)
            != light_background;
        let changed = suspended_changed
            || known_changed
            || mode_changed
            || connection_changed
            || availability_changed
            || theme_changed
            || secure_changed
            || secure_policy_changed;
        if !changed {
            return Ok(());
        }
        let sink = self
            .button
            .sink
            .try_lock()
            .map_err(|_| Error::from_hresult(E_FAIL))?
            .clone();
        // OnUpdate may synchronously call back into GetIcon/GetStatus. No locks.
        if let Some(sink) = sink {
            unsafe {
                sink.OnUpdate((TF_LBI_ICON | TF_LBI_TEXT | TF_LBI_TOOLTIP | TF_LBI_STATUS) as u32)
                    .ok()?;
            }
        }
        Ok(())
    }
}

impl LanguageBar {
    /// 将按钮切换为暂停展示态，同时清除可操作上下文。
    fn suspend(&self) -> Result<()> {
        self.update(None, None, true, true, false, false)
    }
}

impl Drop for LanguageBar {
    fn drop(&mut self) {
        // 停用时先撤销可用性和弱引用，再从 TSF 注销并释放事件接收器。
        self.button.available.store(false, Ordering::Release);
        if let Some(mut target) = boundary::try_teardown(&self.button.target) {
            target.take();
        }
        let item: ITfLangBarItemButton = self.button.to_interface();
        unsafe {
            let _ = self.manager.RemoveItem(&item);
        }
        let sink = boundary::try_teardown(&self.button.sink).and_then(|mut sink| sink.take());
        drop(sink);
    }
}

#[implement(ITfLangBarItemButton, ITfSource)]
struct ModeButton {
    /// 当前焦点上下文的非拥有引用；过期或正在销毁时点击不执行操作。
    target: Mutex<Option<Weak<ContextState>>>,
    /// TSF 外壳通知接收器；最多允许一份订阅。
    sink: Mutex<Option<ITfLangBarItemSink>>,
    /// 最近一次已知的 ASCII 模式值；仅在 `mode_known` 为真时有意义。
    ascii: AtomicBool,
    /// 是否已从当前连接纪元取得可靠输入模式。
    mode_known: AtomicBool,
    /// 与 Rime 的连接是否可用，影响错误提示及图标。
    connected: AtomicBool,
    /// 当前是否有可接收切换命令的上下文。
    available: AtomicBool,
    /// 服务暂停状态；暂停时保留语言栏可见性以供诊断入口使用。
    suspended: AtomicBool,
    /// 当前焦点是否位于安全输入字段。
    secure_field: AtomicBool,
    /// 用户是否允许 Rime 处理安全输入字段；用于明确提示风险。
    allow_rime_in_secure_fields: AtomicBool,
    /// 最近读取的任务栏背景主题，用于选择对比度合适的资源图标。
    light_background: AtomicBool,
    /// 保证 COM 对象存活期间 TIP DLL 不会被卸载。
    _module: ModuleLease,
}

impl ModeButton {
    /// 按暂停、安全字段、未知模式和已知模式的优先级生成紧凑按钮文字。
    fn text(&self) -> &'static str {
        if self.suspended.load(Ordering::Acquire) {
            "!"
        } else if self.secure_field.load(Ordering::Acquire) {
            "密"
        } else if !self.mode_known.load(Ordering::Acquire) {
            "…"
        } else if self.ascii.load(Ordering::Acquire) {
            "英"
        } else {
            "中"
        }
    }
}

impl ITfLangBarItem_Impl for ModeButton_Impl {
    /// 向 TSF 提供稳定的服务 CLSID、标准输入模式 GUID 和按钮样式。
    fn GetInfo(&self, info: *mut TF_LANGBARITEMINFO) -> Result<()> {
        boundary::guard(None, || {
            if info.is_null() {
                return Err(Error::from_hresult(E_POINTER));
            }
            let mut value = TF_LANGBARITEMINFO {
                clsidService: crate::CLSID_WEASEL_TIP,
                guidItem: GUID_LBI_INPUTMODE,
                dwStyle: (TF_LBI_STYLE_BTN_BUTTON | TF_LBI_STYLE_SHOWNINTRAY) as u32,
                ..Default::default()
            };
            let description = HSTRING::from("Weasel-RS 中/英输入模式");
            value.szDescription[..description.len()].copy_from_slice(&description);
            unsafe {
                *info = value;
            }
            Ok(())
        })
    }

    /// 只有存在可用上下文或服务处于暂停展示态时才启用项目。
    fn GetStatus(&self) -> Result<u32> {
        boundary::guard(None, || {
            Ok(
                if self.available.load(Ordering::Acquire) || self.suspended.load(Ordering::Acquire)
                {
                    0
                } else {
                    TF_LBI_STATUS_DISABLED as u32
                },
            )
        })
    }

    /// 本项目不接受 TSF 的显隐切换请求。
    fn Show(&self, _show: BOOL) -> Result<()> {
        boundary::guard(None, || Err(Error::from_hresult(E_NOTIMPL)))
    }

    /// 根据连接、输入模式和安全字段策略生成面向用户的状态提示。
    fn GetTooltipString(&self) -> Result<BSTR> {
        boundary::guard(None, || {
            Ok(BSTR::from(if self.suspended.load(Ordering::Acquire) {
                "输入服务已暂停；Shift＋右键可查看诊断信息"
            } else if self.secure_field.load(Ordering::Acquire) {
                if self.allow_rime_in_secure_fields.load(Ordering::Acquire) {
                    "您选择让 Rime 处理密码输入。如果没有特殊需求，您应该关闭此选项。在设置中了解详情。"
                } else {
                    "正在输入密码"
                }
            } else if !self.connected.load(Ordering::Acquire) {
                "无法连接到 Rime"
            } else if !self.mode_known.load(Ordering::Acquire) {
                "正在同步输入模式"
            } else if self.ascii.load(Ordering::Acquire) {
                "英文模式"
            } else {
                "中文模式"
            }))
        })
    }
}

impl ITfLangBarItemButton_Impl for ModeButton_Impl {
    /// 右键打开命令菜单；左键仅对存活的普通焦点上下文异步发出模式切换。
    ///
    /// 该方法处于 TSF/COM 调用边界，绝不等待 RPC 响应；锁采用 `try_lock`，锁冲突以
    /// HRESULT 错误返回。安全字段、暂停态、失效上下文均禁止切换。
    fn OnClick(&self, click: TfLBIClick, point: &POINT, _area: *const RECT) -> Result<()> {
        boundary::guard(None, || {
            if click == TF_LBI_CLK_RIGHT {
                return show_command_menu(point);
            }
            if click != TF_LBI_CLK_LEFT
                || !self.available.load(Ordering::Acquire)
                || self.suspended.load(Ordering::Acquire)
                || self.secure_field.load(Ordering::Acquire)
            {
                return Ok(());
            }
            let target = self
                .target
                .try_lock()
                .map_err(|_| Error::from_hresult(E_FAIL))?
                .as_ref()
                .and_then(Weak::upgrade);
            let Some(target) = target else {
                return Ok(());
            };
            if !target.alive.load(Ordering::Acquire) {
                return Ok(());
            }
            let command = ContextCommand {
                token: Some(target.token()?),
                action: ContextAction::ToggleAscii as i32,
                ascii_mode: None,
            };
            // Never wait for a pipe response from a language-bar COM callback.
            let rpc = target
                .rpc
                .try_lock()
                .map_err(|_| Error::from_hresult(E_FAIL))?;
            if rpc.context_command(command).is_err() {
                return Err(Error::from_hresult(E_FAIL));
            }
            Ok(())
        })
    }
    /// 菜单内容由原生命令菜单实现提供，此接口入口保持为空操作。
    fn InitMenu(&self, _menu: Ref<'_, ITfMenu>) -> Result<()> {
        boundary::guard(None, || Ok(()))
    }
    /// 将 TSF 菜单选择交给统一的命令校验和分发逻辑。
    fn OnMenuSelect(&self, id: u32) -> Result<()> {
        boundary::guard(None, || dispatch_menu_command(id))
    }
    /// 返回原子状态对应的单字按钮标签。
    fn GetText(&self) -> Result<BSTR> {
        boundary::guard(None, || Ok(BSTR::from(self.text())))
    }
    /// 按当前系统主题和输入状态载入 TIP DLL 内的图标资源。
    ///
    /// 每次外壳查询时重新采样主题；返回的图标不是共享句柄，所有权按 TSF 图标接口约定
    /// 交给调用方释放。
    fn GetIcon(&self) -> Result<HICON> {
        boundary::guard(None, || {
            // Re-read the system theme when the shell requests an icon.
            let light_background = taskbar_is_light();
            self.light_background
                .store(light_background, Ordering::Release);
            load_mode_icon(
                self.connected.load(Ordering::Acquire),
                self.ascii.load(Ordering::Acquire),
                light_background,
                self.mode_known.load(Ordering::Acquire),
                self.secure_field.load(Ordering::Acquire),
                self.allow_rime_in_secure_fields.load(Ordering::Acquire),
            )
        })
    }
}

impl ITfSource_Impl for ModeButton_Impl {
    /// 仅接受一个 `ITfLangBarItemSink` 订阅，并返回固定 cookie。
    fn AdviseSink(&self, iid: *const GUID, unknown: Ref<'_, IUnknown>) -> Result<u32> {
        boundary::guard(None, || {
            if iid.is_null() {
                return Err(Error::from_hresult(E_POINTER));
            }
            if unsafe { *iid } != ITfLangBarItemSink::IID {
                return Err(Error::from_hresult(HRESULT(CONNECT_E_CANNOTCONNECT)));
            }
            let sink: ITfLangBarItemSink = unknown.ok()?.cast()?;
            let mut current = self
                .sink
                .try_lock()
                .map_err(|_| Error::from_hresult(E_FAIL))?;
            if current.is_some() {
                return Err(Error::from_hresult(HRESULT(CONNECT_E_ADVISELIMIT)));
            }
            *current = Some(sink);
            Ok(SINK_COOKIE)
        })
    }
    /// 校验订阅 cookie 后移除接收器；无对应订阅时返回 TSF 连接错误。
    fn UnadviseSink(&self, cookie: u32) -> Result<()> {
        boundary::guard(None, || {
            if cookie != SINK_COOKIE {
                return Err(Error::from_hresult(E_INVALIDARG));
            }
            let sink = self
                .sink
                .try_lock()
                .map_err(|_| Error::from_hresult(E_FAIL))?
                .take();
            if sink.is_none() {
                return Err(Error::from_hresult(HRESULT(CONNECT_E_NOCONNECTION)));
            }
            drop(sink);
            Ok(())
        })
    }
}

/// 根据连接状态、模式和任务栏底色选择资源名；后缀表示字形颜色。
fn icon_name(connected: bool, ascii: bool, light_background: bool) -> PCWSTR {
    // Asset suffixes describe glyph color, not the target Windows theme.
    if !connected {
        return if light_background {
            w!("ERROR_DARK")
        } else {
            w!("ERROR_LIGHT")
        };
    }
    match (ascii, light_background) {
        (true, true) => w!("EN_DARK"),
        (true, false) => w!("EN_LIGHT"),
        (false, true) => w!("ZH_DARK"),
        (false, false) => w!("ZH_LIGHT"),
    }
}

/// 在普通图标规则上优先处理安全字段，并区分尚未同步的输入模式。
fn mode_icon_name(
    connected: bool,
    ascii: bool,
    light_background: bool,
    known: bool,
    secure_field: bool,
    allow_rime_in_secure_fields: bool,
) -> PCWSTR {
    if secure_field {
        return match (allow_rime_in_secure_fields, light_background) {
            (false, true) => w!("KEY_DARK"),
            (false, false) => w!("KEY_LIGHT"),
            (true, true) => w!("KEY_ALERT_DARK"),
            (true, false) => w!("KEY_ALERT_LIGHT"),
        };
    }
    if connected && !known {
        w!("BRAND")
    } else {
        icon_name(connected, ascii, light_background)
    }
}

/// 从当前 TIP DLL 载入图标，并返回由 TSF 调用方负责销毁的独立句柄。
///
/// 调用点持有 `ModuleLease`，因此按函数地址查询模块句柄期间 DLL 保持映射；不使用
/// `LR_SHARED`，避免共享资源句柄与 TSF 的销毁责任冲突。
fn load_mode_icon(
    connected: bool,
    ascii: bool,
    light_background: bool,
    known: bool,
    secure_field: bool,
    allow_rime_in_secure_fields: bool,
) -> Result<HICON> {
    let mut module = HMODULE::default();
    // GetModuleHandleW(None) would look in the host EXE, not our TIP DLL.
    // The button's ModuleLease already protects this code during the call.
    unsafe {
        GetModuleHandleExW(
            (GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT)
                as u32,
            PCWSTR(load_mode_icon as *const () as *const u16),
            &mut module,
        )
        .ok()?;
    }
    let handle = unsafe {
        LoadImageW(
            Some(module),
            mode_icon_name(
                connected,
                ascii,
                light_background,
                known,
                secure_field,
                allow_rime_in_secure_fields,
            ),
            IMAGE_ICON as u32,
            GetSystemMetrics(SM_CXSMICON).max(1),
            GetSystemMetrics(SM_CYSMICON).max(1),
            0, // No LR_SHARED: TSF must be able to DestroyIcon this handle.
        )
    };
    if handle.0.is_null() {
        return Err(Error::from_thread());
    }
    Ok(HICON(handle.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn button() -> ComObject<ModeButton> {
        ComObject::new(ModeButton {
            target: Mutex::new(None),
            sink: Mutex::new(None),
            ascii: AtomicBool::new(true),
            mode_known: AtomicBool::new(true),
            connected: AtomicBool::new(true),
            available: AtomicBool::new(false),
            suspended: AtomicBool::new(false),
            secure_field: AtomicBool::new(false),
            allow_rime_in_secure_fields: AtomicBool::new(false),
            light_background: AtomicBool::new(taskbar_is_light()),
            _module: ModuleLease::new(),
        })
    }

    #[test]
    fn advertises_modern_input_mode_identity_and_safe_defaults() {
        let button = button();
        let item: ITfLangBarItemButton = button.to_interface();
        let mut info = TF_LANGBARITEMINFO::default();
        unsafe {
            item.GetInfo(&mut info).ok().unwrap();
        }
        assert_eq!(info.guidItem, GUID_LBI_INPUTMODE);
        assert_eq!(info.clsidService, crate::CLSID_WEASEL_TIP);
        assert_ne!(info.dwStyle & TF_LBI_STYLE_SHOWNINTRAY as u32, 0);
        unsafe {
            assert_eq!(item.GetStatus().unwrap(), TF_LBI_STATUS_DISABLED as u32);
            assert_eq!(item.GetText().unwrap(), BSTR::from("英"));
            assert_eq!(item.GetTooltipString().unwrap(), BSTR::from("英文模式"));
            assert_eq!(item.GetInfo(std::ptr::null_mut()), E_POINTER);
        }
        button.ascii.store(false, Ordering::Release);
        assert_eq!(unsafe { item.GetText().unwrap() }, BSTR::from("中"));
    }

    #[test]
    fn suspended_indicator_keeps_menu_access_and_disables_toggle() {
        let button = button();
        button.suspended.store(true, Ordering::Release);
        button.available.store(false, Ordering::Release);
        let item: ITfLangBarItemButton = button.to_interface();
        unsafe {
            assert_eq!(item.GetStatus().unwrap(), 0);
            assert!(String::from_utf16_lossy(&item.GetTooltipString().unwrap()).contains("暂停"));
            item.OnClick(TF_LBI_CLK_LEFT, POINT::default(), ptr::null())
                .unwrap();
        }
    }

    #[test]
    fn rejects_unknown_sink_and_cookie_without_registering_with_tsf() {
        let button = button();
        let source: ITfSource = button.to_interface();
        let unknown: IUnknown = button.to_interface();
        unsafe {
            assert_eq!(
                source
                    .AdviseSink(&IUnknown::IID, &unknown)
                    .unwrap_err()
                    .code(),
                HRESULT(CONNECT_E_CANNOTCONNECT)
            );
            assert_eq!(source.UnadviseSink(99), E_INVALIDARG);
            assert_eq!(
                source.UnadviseSink(SINK_COOKIE),
                HRESULT(CONNECT_E_NOCONNECTION)
            );
        }
    }

    #[test]
    fn chooses_glyph_color_for_taskbar_background() {
        for (ascii, light_background, expected) in [
            (true, true, "EN_DARK"),
            (true, false, "EN_LIGHT"),
            (false, true, "ZH_DARK"),
            (false, false, "ZH_LIGHT"),
        ] {
            assert_eq!(
                unsafe {
                    icon_name(true, ascii, light_background)
                        .to_string()
                        .unwrap()
                },
                expected
            );
        }
    }

    #[test]
    fn disconnected_icons_override_both_input_modes() {
        for ascii in [false, true] {
            for (light, expected) in [(true, "ERROR_DARK"), (false, "ERROR_LIGHT")] {
                assert_eq!(
                    unsafe { icon_name(false, ascii, light).to_string().unwrap() },
                    expected
                );
                assert!(include_str!("../../icons.rc").contains(&format!("{expected} ICON ")));
            }
        }
    }

    #[test]
    fn rejects_unknown_broker_commands_without_sending_messages() {
        for id in [0, u32::MAX, 999] {
            assert_eq!(dispatch_menu_command(id).unwrap_err().code(), E_INVALIDARG);
        }
    }

    #[test]
    fn tooltip_tracks_connection_and_input_mode() {
        let button = button();
        let item: ITfLangBarItemButton = button.to_interface();
        for (connected, ascii, expected) in [
            (false, true, "无法连接到 Rime"),
            (true, true, "英文模式"),
            (true, false, "中文模式"),
            (false, false, "无法连接到 Rime"),
        ] {
            button.connected.store(connected, Ordering::Release);
            button.ascii.store(ascii, Ordering::Release);
            assert_eq!(
                unsafe { item.GetTooltipString().unwrap() },
                BSTR::from(expected)
            );
        }
        button.secure_field.store(true, Ordering::Release);
        button
            .allow_rime_in_secure_fields
            .store(false, Ordering::Release);
        assert_eq!(unsafe { item.GetText().unwrap() }, BSTR::from("密"));
        assert_eq!(
            unsafe { item.GetTooltipString().unwrap() },
            BSTR::from("正在输入密码")
        );
        button
            .allow_rime_in_secure_fields
            .store(true, Ordering::Release);
        assert!(
            String::from_utf16_lossy(&unsafe { item.GetTooltipString().unwrap() })
                .contains("应该关闭此选项")
        );
        for (allow, light, expected) in [
            (false, true, "KEY_DARK"),
            (false, false, "KEY_LIGHT"),
            (true, true, "KEY_ALERT_DARK"),
            (true, false, "KEY_ALERT_LIGHT"),
        ] {
            assert_eq!(
                unsafe {
                    mode_icon_name(true, false, light, true, true, allow)
                        .to_string()
                        .unwrap()
                },
                expected
            );
            assert!(include_str!("../../icons.rc").contains(&format!("{expected} ICON ")));
        }
    }

    #[test]
    fn unknown_mode_is_neither_english_nor_an_error() {
        let button = button();
        button.mode_known.store(false, Ordering::Release);
        let item: ITfLangBarItemButton = button.to_interface();
        assert_eq!(unsafe { item.GetText().unwrap() }, BSTR::from("…"));
        assert_eq!(
            unsafe { item.GetTooltipString().unwrap() },
            BSTR::from("正在同步输入模式")
        );
        for ascii in [false, true] {
            assert_eq!(
                unsafe {
                    mode_icon_name(true, ascii, true, false, false, false)
                        .to_string()
                        .unwrap()
                },
                "BRAND"
            );
            assert_eq!(
                unsafe {
                    mode_icon_name(false, ascii, true, false, false, false)
                        .to_string()
                        .unwrap()
                },
                "ERROR_DARK"
            );
        }
    }
}

impl TextService {
    /// 同步语言栏展示状态；展示或刷新失败只记日志，不影响宿主文本输入。
    ///
    /// 输入模式仅采用与当前 RPC 连接纪元一致且非零的缓存值，防止重连前的旧状态闪现。
    pub(super) fn refresh_language_bar(&self) -> Result<()> {
        let bar = self.lock(&self.language_bar)?.clone();
        let Some(bar) = bar else {
            return Ok(());
        };
        if self.faulted.load(Ordering::Acquire) {
            return bar.suspend();
        }
        let focus = *self.lock(&self.focused_context)?;
        let state = self
            .lock(&self.contexts)?
            .iter()
            .find(|s| Some(s.id) == focus)
            .cloned();
        let (ascii, connection_failed) = match &state {
            Some(state) => {
                let rpc = self.lock(&state.rpc)?;
                let epoch = rpc.connection_epoch();
                let failed = rpc.connection_failed();
                drop(rpc);
                let ascii = self
                    .lock(&state.input_mode)?
                    .filter(|(e, _)| *e == epoch && epoch != 0)
                    .map(|(_, ascii)| ascii);
                (ascii, failed)
            }
            None => (None, false),
        };
        // Presentation failures must not disable typing in the host.
        let suspended = state
            .as_ref()
            .is_some_and(|state| state.suspended.load(Ordering::Acquire));
        let secure_field = state
            .as_ref()
            .is_some_and(|state| self.secure_field_visible(state));
        let allow_rime_in_secure_fields = state
            .as_ref()
            .map(|state| self.allow_rime_for_secure_field(state))
            .transpose()?
            .unwrap_or(false);
        if let Err(error) = bar.update(
            state.as_ref(),
            ascii,
            connection_failed,
            suspended,
            secure_field,
            allow_rime_in_secure_fields,
        ) {
            self.lock(&self.rpc)?
                .log("warn", format!("language bar update failed: {error}"));
        }
        Ok(())
    }
}
