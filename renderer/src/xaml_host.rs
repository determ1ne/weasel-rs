#![allow(unsafe_op_in_unsafe_fn)]

use std::{
    sync::{Arc, Mutex, mpsc},
    thread,
    time::Duration,
};

use weasel_common::message::{RenderRect, RenderSnapshot, RendererEvent};
use windows_core::Interface;
use windows_strings::{PCWSTR, w};

use crate::{
    bindings::*,
    state::{Mailbox, Owner, same_content},
    theme::{CandidateTheme, RenderTheme},
};

const WM_RENDERER_UPDATE: u32 = WM_APP as u32 + 10;
const WM_RENDERER_QUIT: u32 = WM_APP as u32 + 11;
const WM_RENDERER_THEME: u32 = WM_APP as u32 + 12;
const WINDOW_CLASS: PCWSTR = w!("weasel-rs-renderer");
const ISLAND_WINDOW_PROPERTY: PCWSTR = w!("WeaselRS.Renderer.XamlIsland");

pub enum UiCommand {
    Render(Owner, RenderSnapshot),
    Disconnect(Owner),
    Quit,
}

#[derive(Clone)]
pub struct UiCommandSender {
    mailbox: Arc<Mutex<Mailbox>>,
    thread_id: u32,
}

pub struct UiHandle {
    commands: UiCommandSender,
    pub events: tokio::sync::mpsc::Receiver<(Owner, RendererEvent)>,
    pub finished: tokio::sync::oneshot::Receiver<Result<(), String>>,
    thread: Option<thread::JoinHandle<()>>,
}

#[derive(Clone)]
pub struct EventSender {
    pub owner: Owner,
    sender: tokio::sync::mpsc::Sender<(Owner, RendererEvent)>,
}

fn join_ui_thread(thread: thread::JoinHandle<()>, timeout: Duration) -> Result<(), String> {
    let deadline = std::time::Instant::now() + timeout;
    while !thread.is_finished() {
        if std::time::Instant::now() >= deadline {
            // The standalone renderer exits on this error. Never interrupt a
            // native call or make its RPC shutdown path join without a bound.
            return Err("XAML shutdown timed out; renderer process must exit".into());
        }
        thread::sleep(Duration::from_millis(10));
    }
    thread.join().map_err(|_| "XAML thread panicked".to_owned())
}

impl EventSender {
    pub fn send(&self, event: RendererEvent) {
        let _ = self.sender.try_send((self.owner, event));
    }
}

impl UiHandle {
    pub fn start() -> Result<Self, String> {
        let mailbox = Arc::new(Mutex::new(Mailbox::default()));
        let receiver = mailbox.clone();
        let (event_sender, events) = tokio::sync::mpsc::channel(32);
        let (finished_sender, finished) = tokio::sync::oneshot::channel();
        let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("weasel-renderer-xaml".to_owned())
            .spawn(move || {
                let result = run_ui(
                    receiver,
                    EventSender {
                        owner: 0,
                        sender: event_sender,
                    },
                    ready_sender.clone(),
                );
                if let Err(error) = &result {
                    let _ = ready_sender.try_send(Err(error.clone()));
                }
                let _ = finished_sender.send(result);
            })
            .map_err(|error| format!("could not start XAML thread: {error}"))?;

        let thread_id = match ready_receiver.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(thread_id)) => thread_id,
            Ok(Err(error)) => {
                let _ = thread.join();
                return Err(error);
            }
            Err(error) => {
                mailbox.lock().unwrap().closed = true;
                // A stuck native initialization cannot safely be interrupted. The RPC
                // process exits on this error; do not turn the startup bound into a join.
                if thread.is_finished() {
                    let _ = thread.join();
                }
                return Err(format!("XAML startup failed or timed out: {error}"));
            }
        };
        Ok(Self {
            commands: UiCommandSender { mailbox, thread_id },
            events,
            finished,
            thread: Some(thread),
        })
    }

    pub fn command_sender(&self) -> UiCommandSender {
        self.commands.clone()
    }

    /// Stop accepting snapshots, close native resources on the UI thread and join it.
    pub fn close(&mut self) -> Result<(), String> {
        if let Some(thread) = self.thread.take() {
            let wake = self.commands.send(UiCommand::Quit);
            join_ui_thread(thread, Duration::from_secs(2))?;
            if let Ok(result) = self.finished.try_recv() {
                result?;
            }
            wake?;
        }
        Ok(())
    }
}

impl Drop for UiHandle {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

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

struct TimerGuard(usize);
impl Drop for TimerGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = KillTimer(None, self.0);
        }
    }
}

impl UiCommandSender {
    #[cfg(test)]
    pub(crate) fn without_ui() -> Self {
        Self {
            mailbox: Arc::new(Mutex::new(Mailbox::default())),
            thread_id: 0,
        }
    }

    pub fn send(&self, command: UiCommand) -> Result<(), String> {
        let mut mailbox = self
            .mailbox
            .lock()
            .map_err(|_| "renderer mailbox poisoned")?;
        let message = match command {
            UiCommand::Render(owner, snapshot) => {
                crate::state::validate(&snapshot)?;
                if !mailbox.render(owner, snapshot) {
                    return Ok(());
                }
                WM_RENDERER_UPDATE
            }
            UiCommand::Disconnect(owner) => {
                if !mailbox.disconnect(owner) {
                    return Ok(());
                }
                WM_RENDERER_UPDATE
            }
            UiCommand::Quit => {
                mailbox.closed = true;
                mailbox.pending = None;
                WM_RENDERER_QUIT
            }
        };
        if message == WM_RENDERER_UPDATE && !mailbox.schedule_wake() {
            return Ok(());
        }
        unsafe {
            if !PostThreadMessageW(self.thread_id, message, WPARAM(0), LPARAM(0)).as_bool() {
                mailbox.closed = true;
                return Err("could not wake XAML thread".to_owned());
            }
        }
        Ok(())
    }

    pub fn is_owner(&self, owner: Owner) -> bool {
        self.mailbox
            .lock()
            .is_ok_and(|m| !m.closed && m.owner == Some(owner))
    }
}

struct UiState {
    window: HWND,
    xaml_window: HWND,
    _xaml_manager: WindowsXamlManager,
    _source: DesktopWindowXamlSource,
    root: Border,
    _content: Grid,
    rows: StackPanel,
    quick_action_panel: Border,
    quick_actions: StackPanel,
    _settings: Option<UISettings>,
    _theme_revoker: Option<windows_core::EventRevoker>,
    last_snapshot: Option<RenderSnapshot>,
    theme: CandidateTheme,
    events: EventSender,
    revokers: Vec<windows_core::EventRevoker>,
}

fn run_ui(
    receiver: Arc<Mutex<Mailbox>>,
    events: EventSender,
    ready: mpsc::SyncSender<Result<u32, String>>,
) -> Result<(), String> {
    unsafe {
        RoInitialize(RO_INIT_SINGLETHREADED)
            .ok()
            .map_err(|error| error.to_string())?;
        let result = run_ui_initialized(receiver, events, ready);
        RoUninitialize();
        result
    }
}

unsafe fn run_ui_initialized(
    receiver: Arc<Mutex<Mailbox>>,
    events: EventSender,
    ready: mpsc::SyncSender<Result<u32, String>>,
) -> Result<(), String> {
    let _previous_dpi = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    let window = create_window()?;
    let _window_guard = WindowGuard(window);
    apply_dwm_corner_preference(window);
    let xaml_manager = WindowsXamlManager::InitializeForCurrentThread()
        .map_err(|error| format!("WindowsXamlManager initialization failed: {error}"))?;
    let _manager_guard = ManagerGuard(xaml_manager.clone());
    let source = DesktopWindowXamlSource::new()
        .map_err(|error| format!("DesktopWindowXamlSource creation failed: {error}"))?;
    let _source_guard = SourceGuard(source.clone());
    let native = source
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
    let _island_window_guard = IslandWindowGuard(window);
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
    source
        .SetContent(&root)
        .map_err(|error| format!("XAML root attachment failed: {error}"))?;

    let thread_id = GetCurrentThreadId();
    // Guarantees that close observes the shared flag even if posting a wake fails.
    let timer = SetTimer(None, 0, 100, None);
    if timer == 0 {
        return Err("could not create UI shutdown timer".into());
    }
    let _timer_guard = TimerGuard(timer);
    let (settings, theme_revoker) = match UISettings::new() {
        Ok(settings) => {
            let revoker = settings.ColorValuesChanged(move |_, _| unsafe {
                let _ = PostThreadMessageW(thread_id, WM_RENDERER_THEME, WPARAM(0), LPARAM(0));
            });
            (Some(settings), revoker.ok())
        }
        Err(_) => (None, None),
    };
    ready
        .send(Ok(thread_id))
        .map_err(|_| "renderer startup handshake failed".to_owned())?;
    let mut state = UiState {
        window,
        xaml_window,
        _xaml_manager: xaml_manager,
        _source: source,
        root,
        _content: content,
        rows,
        quick_action_panel,
        quick_actions,
        _settings: settings,
        _theme_revoker: theme_revoker,
        last_snapshot: None,
        theme: CandidateTheme::default(),
        events,
        revokers: Vec::new(),
    };

    let mut message = MSG::default();
    loop {
        if receiver
            .lock()
            .map_err(|_| "renderer mailbox poisoned")?
            .closed
        {
            break;
        }
        let status = GetMessageW(&mut message, None, 0, 0).0;
        if status == -1 {
            return Err("GetMessageW failed".into());
        }
        if status == 0 {
            break;
        }
        match message.message {
            WM_RENDERER_UPDATE => {
                let mut mailbox = receiver.lock().map_err(|_| "renderer mailbox poisoned")?;
                if let Some((owner, snapshot)) = mailbox.take_pending() {
                    if state.events.owner != owner {
                        state.last_snapshot = None;
                    }
                    state.events.owner = owner;
                    if let Some(snapshot) = snapshot {
                        render_snapshot(&mut state, &snapshot)?;
                        state.last_snapshot = Some(snapshot);
                    } else {
                        state.last_snapshot = None;
                        state.revokers.clear();
                        let _ = ShowWindow(state.window, SW_HIDE);
                    }
                }
            }
            WM_RENDERER_THEME => {
                let mailbox = receiver.lock().map_err(|_| "renderer mailbox poisoned")?;
                state.theme = CandidateTheme::default();
                if mailbox.owner == Some(state.events.owner) {
                    if let Some(snapshot) = state.last_snapshot.take() {
                        render_snapshot(&mut state, &snapshot)?;
                        state.last_snapshot = Some(snapshot);
                    }
                }
            }
            WM_RENDERER_QUIT => {
                break;
            }
            _ => {
                DispatchMessageW(&message);
            }
        }
    }

    Ok(())
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

fn render_snapshot(state: &mut UiState, snapshot: &RenderSnapshot) -> Result<(), String> {
    let Some(anchor) = snapshot.anchor.as_ref().filter(|anchor| anchor.valid) else {
        unsafe {
            let _ = ShowWindow(state.window, SW_HIDE);
        }
        return Ok(());
    };
    if !snapshot.visible || snapshot.items.is_empty() {
        unsafe {
            let _ = ShowWindow(state.window, SW_HIDE);
        }
        return Ok(());
    }

    if !state.last_snapshot.as_ref().is_some_and(|old| {
        old.visible && same_content(old, snapshot) && old.anchor.as_ref().is_some_and(|a| a.valid)
    }) {
        state.revokers.clear();
        apply_dwm_theme(state);
        if let Err(error) = state.theme.render(
            &state.root,
            &state.rows,
            &state.quick_action_panel,
            &state.quick_actions,
            snapshot,
            &state.events,
            &mut state.revokers,
        ) {
            return Err(format!("theme update failed: {error}"));
        }
    }

    let (width, height) = desired_size(state);
    let (x, y) = popup_position(anchor, width, height);
    unsafe {
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

fn desired_size(state: &UiState) -> (i32, i32) {
    let _ = state.root.Measure(Size {
        Width: 10_000.0,
        Height: 10_000.0,
    });
    let desired = state.root.DesiredSize().unwrap_or(Size {
        Width: 260.0,
        Height: 34.0,
    });
    let dpi = unsafe { GetDpiForWindow(state.window).max(96) } as f32;
    (
        (desired.Width * dpi / 96.0).ceil() as i32,
        (desired.Height * dpi / 96.0).ceil() as i32,
    )
}

fn popup_position(anchor: &RenderRect, width: i32, height: i32) -> (i32, i32) {
    unsafe {
        let anchor_rect = RECT {
            left: anchor.left,
            top: anchor.top,
            right: anchor.right,
            bottom: anchor.bottom,
        };
        let monitor = MonitorFromRect(&anchor_rect, MONITOR_DEFAULTTONEAREST as u32);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if monitor.0.is_null() || !GetMonitorInfoW(monitor, &mut info).as_bool() {
            return (anchor.left, anchor.bottom);
        }
        let work = info.rcWork;
        let mut x = anchor.left;
        let mut y = anchor.bottom;
        if y + height > work.bottom {
            y = anchor.top - height;
        }
        if x + width > work.right {
            x = work.right - width;
        }
        x = x.max(work.left);
        y = y.max(work.top);
        (x, y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stalled_ui_shutdown_has_a_deadline_without_interrupting_the_thread() {
        let (release, blocked) = mpsc::channel();
        let (completed, done) = mpsc::channel();
        let worker = thread::spawn(move || {
            blocked.recv_timeout(Duration::from_secs(5)).unwrap();
            completed.send(()).unwrap();
        });
        assert!(join_ui_thread(worker, Duration::from_millis(20)).is_err());
        release.send(()).unwrap();
        done.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(join_ui_thread(thread::spawn(|| {}), Duration::from_secs(1)).is_ok());
    }

    #[test]
    fn event_callbacks_keep_their_owner_and_queue_is_bounded() {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(2);
        let mut current = EventSender { owner: 1, sender };
        let old_callback = current.clone();
        current.owner = 2;
        old_callback.send(RendererEvent::default());
        current.send(RendererEvent::default());
        current.send(RendererEvent::default());
        assert_eq!(receiver.try_recv().unwrap().0, 1);
        assert_eq!(receiver.try_recv().unwrap().0, 2);
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn event_routing_rejects_superseded_and_disconnected_owners() {
        let mailbox = Arc::new(Mutex::new(Mailbox::default()));
        let commands = UiCommandSender {
            mailbox: mailbox.clone(),
            thread_id: 0,
        };
        let snapshot = RenderSnapshot {
            visible: true,
            sequence: 1,
            ..Default::default()
        };
        mailbox.lock().unwrap().render(1, snapshot.clone());
        assert!(commands.is_owner(1));
        mailbox.lock().unwrap().render(2, snapshot);
        assert!(!commands.is_owner(1));
        assert!(commands.is_owner(2));
        mailbox.lock().unwrap().disconnect(2);
        assert!(!commands.is_owner(2));
    }
}
