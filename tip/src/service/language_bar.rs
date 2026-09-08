//! Modern TSF input-mode button. Like Mozc, use GUID_LBI_INPUTMODE and
//! notify the shell through ITfLangBarItemSink, not a separate tray icon.
use super::*;
use crate::bindings::*;
use crate::icons::taskbar_is_light;
use std::sync::Weak;
use weasel_common::message::{ContextAction, ContextCommand};
use windows_core::{ComObject, HRESULT};
use windows_strings::{BSTR, HSTRING, PCWSTR, w};

const SINK_COOKIE: u32 = 1;

// Like Mozc's toggle button, keep left-click mode switching and create a
// native right-click menu. Never hold service locks across the modal menu.
fn show_broker_menu(point: &POINT) -> Result<()> {
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
    for (id, label) in weasel_common::broker_menu::items(weasel_common::about::shift_pressed()) {
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
        send_broker_command(command.0 as u32)?;
    }
    Ok(())
}

fn send_broker_command(command: u32) -> Result<()> {
    if matches!(
        command,
        weasel_common::broker_menu::ABOUT | weasel_common::broker_menu::DIAGNOSTICS
    ) {
        crate::diagnostics::show_dialog(command == weasel_common::broker_menu::DIAGNOSTICS);
        return Ok(());
    }
    if !weasel_common::broker_menu::is_command(command) {
        return Err(Error::from_hresult(E_INVALIDARG));
    }
    let class = HSTRING::from(weasel_common::broker_menu::WINDOW_CLASS);
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
    manager: ITfLangBarItemMgr,
    button: ComObject<ModeButton>,
}

impl LanguageBar {
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
            light_background: AtomicBool::new(taskbar_is_light()),
            _module: ModuleLease::new(),
        });
        let item: ITfLangBarItemButton = button.to_interface();
        unsafe { manager.AddItem(&item).ok()? };
        Ok(Self { manager, button })
    }

    pub fn update(
        &self,
        context: Option<&Arc<ContextState>>,
        ascii: Option<bool>,
        connection_failed: bool,
        suspended: bool,
    ) -> Result<()> {
        let suspended_changed =
            self.button.suspended.swap(suspended, Ordering::AcqRel) != suspended;
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
            || theme_changed;
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
    fn suspend(&self) -> Result<()> {
        self.update(None, None, true, true)
    }
}

impl Drop for LanguageBar {
    fn drop(&mut self) {
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
    target: Mutex<Option<Weak<ContextState>>>,
    sink: Mutex<Option<ITfLangBarItemSink>>,
    ascii: AtomicBool,
    mode_known: AtomicBool,
    connected: AtomicBool,
    available: AtomicBool,
    suspended: AtomicBool,
    light_background: AtomicBool,
    _module: ModuleLease,
}

impl ModeButton {
    fn text(&self) -> &'static str {
        if self.suspended.load(Ordering::Acquire) {
            "!"
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

    fn Show(&self, _show: BOOL) -> Result<()> {
        boundary::guard(None, || Err(Error::from_hresult(E_NOTIMPL)))
    }

    fn GetTooltipString(&self) -> Result<BSTR> {
        boundary::guard(None, || {
            Ok(BSTR::from(if self.suspended.load(Ordering::Acquire) {
                "输入服务已暂停；Shift＋右键可查看诊断信息"
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
    fn OnClick(&self, click: TfLBIClick, point: &POINT, _area: *const RECT) -> Result<()> {
        boundary::guard(None, || {
            if click == TF_LBI_CLK_RIGHT {
                return show_broker_menu(point);
            }
            if click != TF_LBI_CLK_LEFT
                || !self.available.load(Ordering::Acquire)
                || self.suspended.load(Ordering::Acquire)
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
    fn InitMenu(&self, _menu: Ref<'_, ITfMenu>) -> Result<()> {
        boundary::guard(None, || Ok(()))
    }
    fn OnMenuSelect(&self, id: u32) -> Result<()> {
        boundary::guard(None, || send_broker_command(id))
    }
    fn GetText(&self) -> Result<BSTR> {
        boundary::guard(None, || Ok(BSTR::from(self.text())))
    }
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
            )
        })
    }
}

impl ITfSource_Impl for ModeButton_Impl {
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

fn mode_icon_name(connected: bool, ascii: bool, light_background: bool, known: bool) -> PCWSTR {
    if connected && !known {
        w!("BRAND")
    } else {
        icon_name(connected, ascii, light_background)
    }
}

fn load_mode_icon(
    connected: bool,
    ascii: bool,
    light_background: bool,
    known: bool,
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
            mode_icon_name(connected, ascii, light_background, known),
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
            assert_eq!(send_broker_command(id).unwrap_err().code(), E_INVALIDARG);
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
                    mode_icon_name(true, ascii, true, false)
                        .to_string()
                        .unwrap()
                },
                "BRAND"
            );
            assert_eq!(
                unsafe {
                    mode_icon_name(false, ascii, true, false)
                        .to_string()
                        .unwrap()
                },
                "ERROR_DARK"
            );
        }
    }
}

impl TextService {
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
        if let Err(error) = bar.update(state.as_ref(), ascii, connection_failed, suspended) {
            self.lock(&self.rpc)?
                .log("warn", format!("language bar update failed: {error}"));
        }
        Ok(())
    }
}
