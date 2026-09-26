#![allow(unsafe_op_in_unsafe_fn)]

//! Windows 11 候选窗主题的原生窗口与 XAML Island 后端。
//!
//! 后端在 UI 运行时已初始化的 STA 线程上创建、使用并销毁窗口及 XAML
//! 对象；实时模式保持非激活，预览模式则可由用户激活和关闭。

mod config;
mod visual;

use crate::bindings::Windows::Win32::SWP_NOSIZE;
use crate::theme_api::CandidateView;
use windows_core::Interface;
use windows_strings::{PCWSTR, w};
use windows_version::OsVersion;

use crate::{
    bindings::*,
    presentation::{is_visible, popup_position, preview_position},
    theme_api::EventSink,
    theme_api::same_content,
    theme_api::{ThemeBackend, UiMode},
};
use visual::CandidateTheme;

const WINDOW_CLASS: PCWSTR = w!("weasel-rs-renderer");
const ISLAND_WINDOW_PROPERTY: PCWSTR = w!("WeaselRS.Renderer.XamlIsland");

/// 以 RAII 方式持有宿主窗口；始终在其他 XAML 资源完成清理后销毁 HWND。
struct WindowGuard(HWND);
impl Drop for WindowGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.0);
        }
    }
}
/// 持有当前 STA 线程的 XAML 管理器，并在离开作用域时关闭它。
struct ManagerGuard(WindowsXamlManager);
impl Drop for ManagerGuard {
    fn drop(&mut self) {
        let _ = self.0.Close();
    }
}
/// 持有 Island 源；关闭前先解除其对根视觉树的引用。
struct SourceGuard(DesktopWindowXamlSource);
impl Drop for SourceGuard {
    fn drop(&mut self) {
        // All externally held controls have already been released. Detach the
        // source's remaining tree reference before closing the Island itself.
        let _ = self.0.SetContent(None::<&Windows::UI::Xaml::UIElement>);
        let _ = self.0.Close();
    }
}
/// 暂存宿主窗口到 Island 子窗口的关联，确保源关闭前移除该关联。
///
/// 它在 `SourceGuard` 之后创建，因此按局部变量逆序释放时会先清理属性；
/// 初始化中途失败时也遵循相同顺序。
struct IslandWindowGuard(HWND);
impl Drop for IslandWindowGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = RemovePropW(self.0, ISLAND_WINDOW_PROPERTY);
        }
    }
}

/// 单个主题实例的 UI 线程状态及其原生/XAML 资源所有者。
///
/// 字段按声明顺序析构：先撤销事件回调并释放视觉对象，再解除 Island
/// 关联、关闭源和管理器，最后销毁宿主 HWND。该顺序保证 XAML 清理期间宿主
/// 仍有效。实例不可跨线程迁移使用；窗口消息与 XAML 操作须由创建它的 UI
/// STA 线程串行处理。
struct UiState {
    /// 当前视觉树注册的事件撤销句柄；快照重建或隐藏时清空，避免旧回调留存。
    revokers: Vec<windows_core::EventRevoker>,
    /// Island 的根视觉对象，由 `SourceGuard` 同时持有其 XAML 树引用。
    root: Border,
    /// 候选区与快捷操作区的共同布局容器；字段仅用于保持 COM 对象存活。
    _content: Grid,
    /// 候选项容器，每次内容变化时重建其子项。
    rows: StackPanel,
    /// 快捷操作区的边框及分隔线容器。
    quick_action_panel: Border,
    /// 分页和表情快捷操作的容器。
    quick_actions: StackPanel,
    /// 负责候选树、配色及画刷缓存的主题状态。
    theme: CandidateTheme,
    /// 关联属性守卫；须先于 XAML 源析构。
    _island_window_guard: IslandWindowGuard,
    /// Island 源守卫；先脱离根内容再关闭。
    _source_guard: SourceGuard,
    /// 当前线程的 XAML 管理器守卫。
    _manager_guard: ManagerGuard,
    /// 宿主 HWND 的最终所有者，保证它晚于 XAML 资源销毁。
    _window_guard: WindowGuard,
    /// 承载 Island 的宿主窗口句柄。
    window: HWND,
    /// `IDesktopWindowXamlSourceNative::WindowHandle` 返回的 Island 子窗口。
    xaml_window: HWND,
    /// 是否为可激活的独立预览窗口；实时窗口不得抢占宿主焦点。
    preview: bool,
    /// 最近成功提交的候选内容，用于跳过未变化时的视觉树重建。
    last_snapshot: Option<CandidateView>,
    /// 根视觉树在当前 DPI 下测得的 DIP 尺寸缓存；内容或 DPI 变化时失效。
    measured_size: Option<Size>,
}

/// 判断系统版本是否满足该主题对 Windows 11 原生圆角窗口的要求。
///
/// 比较完整版本号，最低支持 Windows 10.0.22000；调用方应在创建任何 XAML
/// 资源之前执行此检查。
fn supports_eleven(version: OsVersion) -> bool {
    version >= OsVersion::new(10, 0, 0, 22000)
}

/// 创建主题后端；不支持的系统会在分配窗口或 XAML 资源前返回错误。
///
/// # 错误
///
/// 返回系统版本不满足要求或原生初始化失败的诊断文本。
fn create(
    mode: UiMode,
    config: config::ThemeConfig,
) -> Result<Box<dyn crate::theme_api::ThemeBackend>, String> {
    // Require Windows 11 for the theme's native rounded window corners.
    // Reject before creating any XAML resources so the next theme can be tried.
    if !supports_eleven(OsVersion::current()) {
        return Err(format!(
            "eleven theme requires Windows 11 (build 22000) or later, but the current version is {}",
            OsVersion::current().build
        ));
    }
    unsafe { create_initialized(mode, config) }
}

#[cfg(test)]
mod version_tests {
    use super::*;
    #[test]
    fn checks_build_not_service_pack() {
        for build in [17763, 18362, 19044, 19045, 21999] {
            assert!(!supports_eleven(OsVersion::new(10, 0, 0, build)));
        }
        assert!(supports_eleven(OsVersion::new(10, 0, 0, 22000)));
        assert!(supports_eleven(OsVersion::new(10, 0, 0, 22621)));
    }
}

/// 在当前 UI STA 上逐层创建宿主窗口、XAML 管理器、Island 和主题视觉树。
///
/// 局部 RAII 守卫按依赖关系排列，使任一步失败都能逆序释放已取得的资源；
/// 返回的后端继续拥有这些资源，并须留在当前线程使用和销毁。
///
/// # Safety
///
/// 调用线程必须已初始化为 XAML 所需的 STA，并且后续生命周期内持续处理该
/// 线程的窗口消息。
unsafe fn create_initialized(
    mode: UiMode,
    config: config::ThemeConfig,
) -> Result<Box<dyn ThemeBackend>, String> {
    let window = create_window(mode)?;
    let window_guard = WindowGuard(window);
    apply_dwm_corner_preference(window);
    let xaml_manager = WindowsXamlManager::InitializeForCurrentThread()
        .map_err(|error| format!("WindowsXamlManager initialization failed: {error}"))?;
    let manager_guard = ManagerGuard(xaml_manager);
    let source = DesktopWindowXamlSource::new()
        .map_err(|error| format!("DesktopWindowXamlSource creation failed: {error}"))?;
    let source_guard = SourceGuard(source);
    let native = source_guard
        .0
        .cast::<IDesktopWindowXamlSourceNative>()
        .map_err(|error| format!("XAML native bridge unavailable: {error}"))?;
    native
        .AttachToWindow(window)
        .ok()
        .map_err(|error| format!("XAML source attachment failed: {error}"))?;
    let xaml_window = native
        .WindowHandle()
        .map_err(|error| format!("XAML child window unavailable: {error}"))?;
    if xaml_window.0.is_null()
        || !SetPropW(window, ISLAND_WINDOW_PROPERTY, Some(HANDLE(xaml_window.0))).as_bool()
    {
        return Err("could not associate the XAML Island with its host window".into());
    }
    let island_window_guard = IslandWindowGuard(window);
    let child_exstyle = GetWindowLongPtrW(xaml_window, GWL_EXSTYLE);
    let _ = SetWindowLongPtrW(
        xaml_window,
        GWL_EXSTYLE,
        child_exstyle | WS_EX_NOACTIVATE as isize,
    );
    let root = Border::new().map_err(|error| format!("Border creation failed: {error}"))?;
    let content = Grid::new().map_err(|error| format!("Grid creation failed: {error}"))?;
    let rows = StackPanel::new().map_err(|error| format!("StackPanel creation failed: {error}"))?;
    let quick_action_panel =
        Border::new().map_err(|error| format!("quick action panel creation failed: {error}"))?;
    let quick_actions = StackPanel::new()
        .map_err(|error| format!("quick action stack creation failed: {error}"))?;
    let columns = content
        .ColumnDefinitions()
        .map_err(|error| format!("Grid column collection unavailable: {error}"))?;
    for _ in 0..2 {
        let column = ColumnDefinition::new()
            .map_err(|error| format!("Grid column creation failed: {error}"))?;
        column
            .SetWidth(GridLength {
                Value: 1.0,
                GridUnitType: GridUnitType::Auto,
            })
            .map_err(|error| format!("Grid column sizing failed: {error}"))?;
        columns
            .Append(&column)
            .map_err(|error| format!("Grid column insertion failed: {error}"))?;
    }
    Grid::SetColumn(&rows, 0)
        .map_err(|error| format!("candidate column assignment failed: {error}"))?;
    quick_action_panel
        .SetChild(&quick_actions)
        .map_err(|error| format!("quick action attachment failed: {error}"))?;
    Grid::SetColumn(&quick_action_panel, 1)
        .map_err(|error| format!("quick action column assignment failed: {error}"))?;
    let content_children = content
        .Children()
        .map_err(|error| format!("Grid children unavailable: {error}"))?;
    content_children
        .Append(&rows)
        .map_err(|error| format!("candidate rows attachment failed: {error}"))?;
    content_children
        .Append(&quick_action_panel)
        .map_err(|error| format!("quick action attachment failed: {error}"))?;
    root.SetChild(&content)
        .map_err(|error| format!("XAML content attachment failed: {error}"))?;
    source_guard
        .0
        .SetContent(&root)
        .map_err(|error| format!("XAML root attachment failed: {error}"))?;

    let theme = CandidateTheme::new(config);
    theme
        .prepare(&root, &rows, &quick_action_panel, &quick_actions)
        .map_err(|error| format!("theme preparation failed: {error}"))?;

    Ok(Box::new(UiState {
        revokers: Vec::new(),
        root,
        _content: content,
        rows,
        quick_action_panel,
        quick_actions,
        theme,
        _island_window_guard: island_window_guard,
        _source_guard: source_guard,
        _manager_guard: manager_guard,
        _window_guard: window_guard,
        window,
        xaml_window,
        preview: mode != UiMode::Live,
        last_snapshot: None,
        measured_size: None,
    }))
}

impl ThemeBackend for UiState {
    fn take_notices(&mut self) -> Vec<crate::theme_api::ThemeNotice> {
        self.theme.take_notices()
    }
    fn render(&mut self, snapshot: &CandidateView, events: &EventSink) -> Result<(), String> {
        if !is_visible(snapshot) {
            self.hide();
            return Ok(());
        }
        if let Err(error) = render_snapshot(self, snapshot, events) {
            self.hide();
            return Err(error);
        }
        self.last_snapshot = Some(snapshot.clone());
        Ok(())
    }

    fn hide(&mut self) {
        self.last_snapshot = None;
        self.measured_size = None;
        self.revokers.clear();
        // XAML composition is asynchronous. Remove candidate visuals while the
        // Island is hidden so a later ShowWindow cannot expose the last frame
        // while the replacement tree is still being committed.
        if let Ok(children) = self.rows.Children() {
            let _ = children.Clear();
        }
        if let Ok(children) = self.quick_actions.Children() {
            let _ = children.Clear();
        }
        unsafe {
            let _ = ShowWindow(self.window, SW_HIDE);
        }
    }

    fn refresh_appearance(&mut self) -> Result<(), String> {
        self.theme.refresh();
        self.last_snapshot = None;
        Ok(())
    }
}

/// 注册窗口类并创建实时宿主或可关闭的预览窗口。
///
/// 成功时调用方取得新 HWND 的销毁责任；实时窗口是置顶、工具、非激活窗口，
/// 预览窗口则出现在任务栏并允许激活。创建失败返回错误文本。
///
/// # Safety
///
/// 必须在负责该窗口消息循环的线程中调用。
unsafe fn create_window(mode: UiMode) -> Result<HWND, String> {
    let icon = load_weasel_icon();
    let class = WNDCLASSW {
        lpfnWndProc: Some(window_proc),
        lpszClassName: WINDOW_CLASS,
        // Class icon also acts as the preview task-bar icon fallback.
        hIcon: icon,
        ..Default::default()
    };
    if RegisterClassW(&class).0 == 0 {
        // The class may already exist if the host is reinitialized in-process.
    }
    // The preview keeps the borderless strip identical to input but must be
    // findable and closable: no WS_EX_TOOLWINDOW (so a task-bar button appears)
    // and no WS_EX_NOACTIVATE (so it can be activated and closed). Live keeps the
    // invisible, non-activating tool window.
    let (ex_style, title) = match mode {
        UiMode::Live => (
            (WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE) as u32,
            WINDOW_CLASS,
        ),
        UiMode::Preview => ((WS_EX_TOPMOST) as u32, w!("Weasel-RS 皮肤预览")),
    };
    let window = CreateWindowExW(
        ex_style,
        WINDOW_CLASS,
        title,
        WS_POPUP
            | if mode != UiMode::Live {
                crate::bindings::Windows::Win32::WS_SYSMENU as u32
            } else {
                0
            },
        0,
        0,
        1,
        1,
        None,
        None,
        None,
        None,
    );
    if window.0.is_null() {
        return Err("could not create renderer window".to_owned());
    }
    if mode != UiMode::Live {
        apply_taskbar_icon(window, icon);
    }
    Ok(window)
}

/// 从渲染器所在模块加载内嵌的 Weasel 图标；模块句柄不可用时返回空图标。
///
/// 返回的资源由系统模块持有，不转移销毁责任。
unsafe fn load_weasel_icon() -> HICON {
    let instance = GetModuleHandleW(None);
    if instance.0.is_null() {
        return HICON::default();
    }
    LoadIconW(Some(instance), w!("WEASEL_ICON"))
}

/// 同时设置预览窗口的小图标和大图标，使任务栏按钮显示 Weasel 图标。
///
/// 空图标是无操作；`WM_SETICON` 是任务栏图像的权威设置入口。
unsafe fn apply_taskbar_icon(window: HWND, icon: HICON) {
    if icon.0.is_null() {
        return;
    }
    let _ = SendMessageW(
        window,
        WM_SETICON as u32,
        WPARAM(ICON_SMALL as usize),
        LPARAM(icon.0 as isize),
    );
    let _ = SendMessageW(
        window,
        WM_SETICON as u32,
        WPARAM(ICON_BIG as usize),
        LPARAM(icon.0 as isize),
    );
}

/// 处理预览关闭、宿主尺寸变化和 DPI 变化，并将其余消息交给默认窗口过程。
///
/// `WM_CLOSE` 只为可激活的预览窗口结束消息循环；实时窗口由后端关闭流程销毁。
/// 尺寸消息仅调整已关联的 Island 子窗口，避免显示内部 CoreWindow 干扰输入。
///
/// # Safety
///
/// 参数由 Windows 窗口过程按系统 ABI 提供；DPI 变化消息中的 `lparam` 必须指向
/// 有效的建议矩形，且本过程只能在该 HWND 所属线程执行。
unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // The preview window (the only non-tool-window variant) ends the process when
    // the user closes it. Live tool windows are destroyed by the shutdown path.
    if message == WM_CLOSE as u32
        && GetWindowLongPtrW(window, GWL_EXSTYLE) & WS_EX_TOOLWINDOW as isize == 0
    {
        PostQuitMessage(0);
        // Drop XAML resources before the WindowGuard destroys HWND.
        return LRESULT(0);
    }
    if message == WM_SIZE as u32 {
        let width = (lparam.0 as u32 & 0xffff) as i32;
        let height = ((lparam.0 as u32 >> 16) & 0xffff) as i32;
        // XAML's first child can be an internal CoreWindow on Windows 10.
        // Only resize the HWND returned by WindowHandle(); showing CoreWindow
        // over the Island intercepts its input. No target exists before attach
        // or after the association guard has been dropped.
        let child = HWND(GetPropW(window, ISLAND_WINDOW_PROPERTY).0);
        if !child.0.is_null() {
            let _ = SetWindowPos(
                child,
                None,
                0,
                0,
                width,
                height,
                (SWP_NOACTIVATE | SWP_NOZORDER | SWP_SHOWWINDOW) as u32,
            );
        }
        return LRESULT(0);
    }
    if message == WM_DPICHANGED as u32 {
        let suggested = &*(lparam.0 as *const RECT);
        let _ = SetWindowPos(
            window,
            Some(HWND_TOPMOST),
            suggested.left,
            suggested.top,
            suggested.right - suggested.left,
            suggested.bottom - suggested.top,
            SWP_NOACTIVATE as u32,
        );
        return LRESULT(0);
    }
    DefWindowProcW(window, message, wparam, lparam)
}

/// 将候选快照提交到视觉树，并按预览/实时模式调整宿主及 Island 的位置和大小。
///
/// 内容未变时复用已建视觉树，只更新位置；跨监视器移动若改变 DPI，则丢弃
/// 尺寸缓存并重新测量。锚点缺失或视觉更新失败会返回错误，由上层隐藏窗口；
/// 实时模式始终使用不激活窗口的定位标志。
fn render_snapshot(
    state: &mut UiState,
    snapshot: &CandidateView,
    events: &EventSink,
) -> Result<(), String> {
    // render() has checked visibility; retain fallible extraction without a panic.
    let anchor = snapshot
        .anchor
        .as_ref()
        .ok_or("visible snapshot has no anchor")?;
    let preview = state.preview;

    let unchanged = state
        .last_snapshot
        .as_ref()
        .is_some_and(|old| same_content(old, snapshot));
    if !unchanged {
        state.revokers.clear();
        state.measured_size = None;
        apply_dwm_theme(state);
        if let Err(error) = state.theme.render(
            &state.root,
            &state.rows,
            &state.quick_action_panel,
            &state.quick_actions,
            snapshot,
            events,
            &mut state.revokers,
        ) {
            return Err(format!("theme update failed: {error}"));
        }
    }

    unsafe {
        if unchanged {
            let (width, height) = desired_size(state);
            let (x, y) = if preview {
                preview_position(width, height)
            } else {
                popup_position(anchor, width, height)
            };
            let dpi = GetDpiForWindow(state.window);
            let _ = SetWindowPos(
                state.window,
                Some(HWND_TOPMOST),
                x,
                y,
                0,
                0,
                (SWP_NOACTIVATE | SWP_NOSIZE) as u32,
            );
            if GetDpiForWindow(state.window) == dpi {
                return Ok(());
            }
            // A cross-monitor move can synchronously change the Island DPI.
            // Invalidate the cache and remeasure before resizing both HWNDs.
            state.measured_size = None;
        }
        let (width, height) = desired_size(state);
        let (x, y) = if preview {
            preview_position(width, height)
        } else {
            popup_position(anchor, width, height)
        };
        // The preview window is a normal activatable window; the live strip must
        // never steal focus from the host application.
        let show_flags = if preview {
            SWP_SHOWWINDOW as u32
        } else {
            (SWP_NOACTIVATE | SWP_SHOWWINDOW) as u32
        };
        let _ = SetWindowPos(
            state.window,
            Some(HWND_TOPMOST),
            x,
            y,
            width,
            height,
            show_flags,
        );
        let child_flags = if preview {
            (SWP_NOZORDER | SWP_SHOWWINDOW) as u32
        } else {
            (SWP_NOACTIVATE | SWP_NOZORDER | SWP_SHOWWINDOW) as u32
        };
        let _ = SetWindowPos(state.xaml_window, None, 0, 0, width, height, child_flags);
        let _ = ShowWindow(state.window, SW_SHOWNA);
    }
    Ok(())
}

/// 将系统浅色/深色判断应用于宿主与 Island 的 DWM 深色标题栏属性。
///
/// 系统设置读取失败时直接跳过；DWM 属性设置结果不作为渲染失败传播。
fn apply_dwm_theme(state: &UiState) {
    let Ok(settings) = UISettings::new() else {
        return;
    };
    let Ok(background) = settings.GetColorValue(UIColorType::Background) else {
        return;
    };
    let luminance = 299_u32 * background.R as u32
        + 587_u32 * background.G as u32
        + 114_u32 * background.B as u32;
    let dark_mode: i32 = (luminance < 128_000).into();
    unsafe {
        let value = &dark_mode as *const i32 as *const core::ffi::c_void;
        let size = std::mem::size_of_val(&dark_mode) as u32;
        let _ = DwmSetWindowAttribute(
            state.window,
            DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
            value,
            size,
        );
        let _ = DwmSetWindowAttribute(
            state.xaml_window,
            DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
            value,
            size,
        );
    }
}

/// 请求 DWM 为宿主窗口使用圆角；系统拒绝时记录诊断但不阻止主题创建。
fn apply_dwm_corner_preference(window: HWND) {
    unsafe {
        let preference = DWMWCP_ROUND;
        let result = DwmSetWindowAttribute(
            window,
            DWMWA_WINDOW_CORNER_PREFERENCE as u32,
            &preference as *const _ as *const core::ffi::c_void,
            std::mem::size_of_val(&preference) as u32,
        );
        if let Err(error) = result.ok() {
            crate::diagnostics::record(format_args!(
                "DWM rounded-corner preference rejected: {error}"
            ));
        }
    }
}

/// 获取根视觉树在当前窗口 DPI 下的像素尺寸，并缓存 DIP 测量结果。
///
/// 缓存只保存 XAML 的期望 DIP 尺寸，返回前按窗口 DPI 向上取整为像素；测量
/// 失败时采用稳定的默认尺寸。调用方须在内容或 DPI 改变后清除缓存。
fn desired_size(state: &mut UiState) -> (i32, i32) {
    let desired = *state.measured_size.get_or_insert_with(|| {
        let _ = state.root.Measure(Size {
            Width: 10_000.0,
            Height: 10_000.0,
        });
        state.root.DesiredSize().unwrap_or(Size {
            Width: 260.0,
            Height: 34.0,
        })
    });
    let dpi = unsafe { GetDpiForWindow(state.window).max(96) } as f32;
    (
        (desired.Width * dpi / 96.0).ceil() as i32,
        (desired.Height * dpi / 96.0).ceil() as i32,
    )
}

/// eleven 主题的工厂入口，由主题 ABI 导出宏调用。
pub struct Factory;

impl crate::theme_api::ThemeFactory for Factory {
    fn default_settings(&self) -> Result<serde_json::Value, String> {
        serde_json::from_str(include_str!("config.json")).map_err(|e| e.to_string())
    }
    fn name(&self) -> &'static str {
        "eleven"
    }
    fn capabilities(&self) -> crate::theme_api::ThemeCapabilities {
        crate::theme_api::ThemeCapabilities::CANDIDATES_ONLY
    }
    fn create(
        &self,
        mode: UiMode,
        settings: &weasel_common::settings::ConfigSnapshot,
    ) -> crate::theme_api::ThemeCreation {
        let config = config::ThemeConfig::load(settings);
        // Keep the notice buffer alive if native initialization fails midway.
        let backend = create(mode, config.clone());
        let notices = config.take_notices();
        crate::theme_api::ThemeCreation { backend, notices }
    }
}
