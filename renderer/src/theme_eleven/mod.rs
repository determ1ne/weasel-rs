#![allow(unsafe_op_in_unsafe_fn)]

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

struct WindowGuard(HWND);
impl Drop for WindowGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.0);
        }
    }
}
struct ManagerGuard(WindowsXamlManager);
impl Drop for ManagerGuard {
    fn drop(&mut self) {
        let _ = self.0.Close();
    }
}
struct SourceGuard(DesktopWindowXamlSource);
impl Drop for SourceGuard {
    fn drop(&mut self) {
        // All externally held controls have already been released. Detach the
        // source's remaining tree reference before closing the Island itself.
        let _ = self.0.SetContent(None::<&Windows::UI::Xaml::UIElement>);
        let _ = self.0.Close();
    }
}
// Declared after SourceGuard during initialization, so the parent association
// is removed before Close() destroys the Island (including on early errors).
struct IslandWindowGuard(HWND);
impl Drop for IslandWindowGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = RemovePropW(self.0, ISLAND_WINDOW_PROPERTY);
        }
    }
}

// Fields drop in declaration order. Revoke callbacks before releasing controls,
// then remove the Island association before closing its source and manager.
// The host HWND remains valid throughout XAML cleanup.
struct UiState {
    revokers: Vec<windows_core::EventRevoker>,
    root: Border,
    _content: Grid,
    rows: StackPanel,
    quick_action_panel: Border,
    quick_actions: StackPanel,
    theme: CandidateTheme,
    _island_window_guard: IslandWindowGuard,
    _source_guard: SourceGuard,
    _manager_guard: ManagerGuard,
    _window_guard: WindowGuard,
    window: HWND,
    xaml_window: HWND,
    preview: bool,
    // Broker configuration (JSON) forwarded to the theme so it can render the
    // user's configured skin. Rendering reads it once theme-specific settings
    // are supported; carried (not yet consumed) for now.
    #[allow(dead_code)]
    theme_settings: String,
    last_snapshot: Option<CandidateView>,
    measured_size: Option<Size>,
}

/// Called and dropped on the UI runtime's initialized STA.
fn supports_xaml(version: OsVersion) -> bool {
    version >= OsVersion::new(10, 0, 0, 18362)
}

fn create(
    mode: UiMode,
    theme_settings: &str,
) -> Result<Box<dyn crate::theme_api::ThemeBackend>, String> {
    // The XAML Island backend requires Windows 10 1903 (build 18362) or later.
    if !supports_xaml(OsVersion::current()) {
        return Err(format!(
            "XAML Island backend requires Windows 10 1903 (build 18362) or later, but the current version is {}",
            OsVersion::current().build
        ));
    }
    unsafe { create_initialized(mode, theme_settings) }
}

#[cfg(test)]
mod version_tests {
    use super::*;
    #[test]
    fn checks_build_not_service_pack() {
        assert!(!supports_xaml(OsVersion::new(10, 0, 0, 17763)));
        assert!(supports_xaml(OsVersion::new(10, 0, 0, 18362)));
        assert!(supports_xaml(OsVersion::new(10, 0, 0, 19044)));
        assert!(supports_xaml(OsVersion::new(10, 0, 0, 22621)));
    }
}

unsafe fn create_initialized(
    mode: UiMode,
    theme_settings: &str,
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

    let theme = CandidateTheme::default();
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
        preview: mode == UiMode::Preview,
        theme_settings: theme_settings.to_owned(),
        last_snapshot: None,
        measured_size: None,
    }))
}

impl ThemeBackend for UiState {
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
        self.revokers.clear();
        unsafe {
            let _ = ShowWindow(self.window, SW_HIDE);
        }
    }

    fn refresh_appearance(&mut self) -> Result<(), String> {
        self.theme = CandidateTheme::default();
        self.last_snapshot = None;
        Ok(())
    }
}

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
            | if mode == UiMode::Preview {
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
    if mode == UiMode::Preview {
        apply_taskbar_icon(window, icon);
    }
    Ok(window)
}

/// Loads the Weasel icon resource embedded in the renderer executable.
unsafe fn load_weasel_icon() -> HICON {
    let instance = GetModuleHandleW(None);
    if instance.0.is_null() {
        return HICON::default();
    }
    LoadIconW(Some(instance), w!("WEASEL_ICON"))
}

/// Sets the window's small and big icons so the preview window's task-bar button
/// shows the Weasel icon. WM_SETICON is authoritative for the task-bar image.
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

pub struct Factory;

impl crate::theme_api::ThemeFactory for Factory {
    fn name(&self) -> &'static str {
        "eleven"
    }
    fn capabilities(&self) -> crate::theme_api::ThemeCapabilities {
        crate::theme_api::ThemeCapabilities::CANDIDATES_ONLY
    }
    fn create(&self, mode: UiMode, settings: &str) -> Result<Box<dyn ThemeBackend>, String> {
        create(mode, settings)
    }
}
