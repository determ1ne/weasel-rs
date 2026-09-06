#![allow(unsafe_op_in_unsafe_fn)]

mod visual;

use crate::bindings::Windows::Win32::SWP_NOSIZE;
use weasel_common::message::RenderSnapshot;
use windows_core::Interface;
use windows_strings::{PCWSTR, w};
use windows_version::OsVersion;

use crate::{
    backend::ThemeBackend,
    bindings::*,
    presentation::{is_visible, popup_position},
    state::same_content,
    ui_runtime::EventSender,
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
    last_snapshot: Option<RenderSnapshot>,
    measured_size: Option<Size>,
}

/// Called and dropped on the UI runtime's initialized STA.
pub fn create() -> Result<Box<dyn crate::backend::ThemeBackend>, String> {
    // The XAML Island backend requires Windows 10 1903 (build 18362) or later.
    const REQUIRED_VERSION: OsVersion = OsVersion::new(10, 0, 0, 18362);
    if REQUIRED_VERSION > OsVersion::current() {
        return Err(format!(
            "XAML Island backend requires Windows 10 1903 (build 18362) or later, but the current version is {}",
            OsVersion::current().pack
        ));
    }
    unsafe { create_initialized() }
}

unsafe fn create_initialized() -> Result<Box<dyn ThemeBackend>, String> {
    let window = create_window()?;
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
        last_snapshot: None,
        measured_size: None,
    }))
}

impl ThemeBackend for UiState {
    fn render(&mut self, snapshot: &RenderSnapshot, events: &EventSender) -> Result<(), String> {
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

unsafe fn create_window() -> Result<HWND, String> {
    let class = WNDCLASSW {
        lpfnWndProc: Some(window_proc),
        lpszClassName: WINDOW_CLASS,
        ..Default::default()
    };
    if RegisterClassW(&class).0 == 0 {
        // The class may already exist if the host is reinitialized in-process.
    }
    let window = CreateWindowExW(
        (WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE) as u32,
        WINDOW_CLASS,
        WINDOW_CLASS,
        WS_POPUP,
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
        return Err("could not create renderer popup window".to_owned());
    }
    Ok(window)
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
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
    snapshot: &RenderSnapshot,
    events: &EventSender,
) -> Result<(), String> {
    // render() has checked visibility; retain fallible extraction without a panic.
    let anchor = snapshot
        .anchor
        .as_ref()
        .ok_or("visible snapshot has no anchor")?;

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

    let (width, height) = desired_size(state);
    let (x, y) = popup_position(anchor, width, height);
    unsafe {
        if unchanged {
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
        let (x, y) = popup_position(anchor, width, height);
        let _ = SetWindowPos(
            state.window,
            Some(HWND_TOPMOST),
            x,
            y,
            width,
            height,
            (SWP_NOACTIVATE | SWP_SHOWWINDOW) as u32,
        );
        let _ = SetWindowPos(
            state.xaml_window,
            None,
            0,
            0,
            width,
            height,
            (SWP_NOACTIVATE | SWP_NOZORDER | SWP_SHOWWINDOW) as u32,
        );
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
