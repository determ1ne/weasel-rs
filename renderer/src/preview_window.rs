//! Native preview controller. Disk/RPC/theme startup never runs in its window proc.
use crate::{d2d_bindings::*, theme_api::UiMode, ui_runtime::UiHandle};
use std::{cell::Cell, sync::mpsc, thread};
use windows_strings::{HSTRING, w};

const COMPLETE: u32 = WM_APP as u32 + 40;
const REFRESH: usize = 1;
const CLOSE: usize = 2;
enum Command {
    Refresh,
    Close,
}
enum Completion {
    Refreshed(Result<String, String>),
    Closed,
}
struct Controller {
    commands: mpsc::Sender<Command>,
    results: mpsc::Receiver<Completion>,
    refresh: Cell<HWND>,
    close: Cell<HWND>,
    status: Cell<HWND>,
    busy: Cell<bool>,
    closing: Cell<bool>,
}

fn worker(hwnd: usize, commands: mpsc::Receiver<Command>, results: mpsc::Sender<Completion>) {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        let mut current: Option<UiHandle> = None;
        while let Ok(command) = commands.recv() {
            if matches!(command, Command::Close) {
                break;
            }
            let result = (|| {
                let settings = runtime.block_on(crate::rpc::load_theme(true))?;
                let requested = settings.theme()?;
                let mut next = UiHandle::start(&requested, UiMode::Preview, &settings)?;
                next.events.close();
                let selected = next.theme;
                let old = current.replace(next);
                drop(old);
                Ok(if selected == requested {
                    format!("当前主题：{selected}")
                } else {
                    format!("主题 {requested} 不可用，已回退到 {selected}")
                })
            })();
            let _ = results.send(Completion::Refreshed(result));
            unsafe {
                let _ = PostMessageW(Some(HWND(hwnd as *mut _)), COMPLETE, WPARAM(0), LPARAM(0));
            }
        }
        drop(current);
        Ok::<_, String>(())
    }));
    if !matches!(outcome, Ok(Ok(()))) {
        let _ = results.send(Completion::Refreshed(Err("预览工作线程异常退出".into())));
    }
    let _ = results.send(Completion::Closed);
    unsafe {
        let _ = PostMessageW(Some(HWND(hwnd as *mut _)), COMPLETE, WPARAM(0), LPARAM(0));
    }
}

impl Controller {
    fn refresh(&self) {
        if self.closing.get() || self.busy.replace(true) {
            return;
        }
        unsafe {
            let _ = EnableWindow(self.refresh.get(), false);
            let _ = SetWindowTextW(self.status.get(), w!("正在读取配置……"));
        }
        if self.commands.send(Command::Refresh).is_err() {
            unsafe {
                PostQuitMessage(1);
            }
        }
    }
    fn close(&self) {
        if self.closing.replace(true) {
            return;
        }
        unsafe {
            let _ = EnableWindow(self.refresh.get(), false);
            let _ = EnableWindow(self.close.get(), false);
            let _ = SetWindowTextW(self.status.get(), w!("正在关闭……"));
        }
        if self.commands.send(Command::Close).is_err() {
            unsafe {
                PostQuitMessage(0);
            }
        }
    }
    unsafe fn layout(&self, hwnd: HWND) {
        unsafe {
            let dpi = GetDpiForWindow(hwnd).max(96) as i32;
            let px = |v| v * dpi / 96;
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            for (child, x, y, width, height) in [
                (
                    self.status.get(),
                    px(16),
                    px(16),
                    (rc.right - px(32)).max(1),
                    px(48),
                ),
                (self.refresh.get(), px(16), px(76), px(96), px(30)),
                (self.close.get(), px(128), px(76), px(96), px(30)),
            ] {
                if !child.0.is_null() {
                    let _ = SetWindowPos(child, None, x, y, width, height, SWP_NOACTIVATE as u32);
                }
            }
        }
    }
}

unsafe extern "system" fn window_proc(hwnd: HWND, message: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        if message == WM_NCCREATE as u32 {
            let create = &*(lp.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
        }
        if let Some(state) = (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Controller).as_ref()
        {
            match message as i32 {
                WM_COMMAND => {
                    match wp.0 & 0xffff {
                        REFRESH => state.refresh(),
                        CLOSE => state.close(),
                        _ => {}
                    }
                    return LRESULT(0);
                }
                WM_CLOSE => {
                    state.close();
                    return LRESULT(0);
                }
                WM_SIZE => {
                    state.layout(hwnd);
                    return LRESULT(0);
                }
                WM_DPICHANGED => {
                    let rc = &*(lp.0 as *const RECT);
                    let _ = SetWindowPos(
                        hwnd,
                        None,
                        rc.left,
                        rc.top,
                        rc.right - rc.left,
                        rc.bottom - rc.top,
                        SWP_NOACTIVATE as u32,
                    );
                    state.layout(hwnd);
                    return LRESULT(0);
                }
                _ if message == COMPLETE => {
                    while let Ok(result) = state.results.try_recv() {
                        match result {
                            Completion::Closed => {
                                PostQuitMessage(0);
                            }
                            Completion::Refreshed(result) => {
                                state.busy.set(false);
                                if state.closing.get() {
                                    continue;
                                }
                                let _ = EnableWindow(state.refresh.get(), true);
                                match result {
                                    Ok(text) => {
                                        let _ = SetWindowTextW(
                                            state.status.get(),
                                            &HSTRING::from(text),
                                        );
                                    }
                                    Err(error) => {
                                        crate::diagnostics::record(format_args!(
                                            "preview refresh failed: {error}"
                                        ));
                                        let _ = SetWindowTextW(
                                            state.status.get(),
                                            w!("刷新失败，原预览保持不变"),
                                        );
                                        MessageBoxW(
                                            Some(hwnd),
                                            &HSTRING::from(error),
                                            w!("小狼毫RS：外观预览"),
                                            (MB_OK | MB_ICONERROR) as u32,
                                        );
                                    }
                                }
                            }
                        }
                    }
                    return LRESULT(0);
                }
                WM_NCDESTROY => {
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                }
                _ => {}
            }
        }
        DefWindowProcW(hwnd, message, wp, lp)
    }))
    .unwrap_or_else(|_| {
        unsafe {
            PostQuitMessage(1);
        }
        LRESULT(0)
    })
}

pub fn run() -> Result<(), String> {
    let (commands, receiver) = mpsc::channel();
    let (sender, results) = mpsc::channel();
    let state = Box::new(Controller {
        commands,
        results,
        refresh: Cell::default(),
        close: Cell::default(),
        status: Cell::default(),
        busy: Cell::new(false),
        closing: Cell::new(false),
    });
    unsafe {
        let _ = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let instance = GetModuleHandleW(None);
        let class = w!("Weasel.Preview.Controller");
        let wc = WNDCLASSW {
            hInstance: instance,
            lpszClassName: class,
            lpfnWndProc: Some(window_proc),
            hCursor: LoadCursorW(None, IDC_ARROW),
            hIcon: LoadIconW(Some(instance), w!("WEASEL_ICON")),
            hbrBackground: GetSysColorBrush(COLOR_BTNFACE),
            ..Default::default()
        };
        if RegisterClassW(&wc).0 == 0 {
            return Err(windows_core::Error::from_thread().to_string());
        }
        let hwnd = CreateWindowExW(
            WS_EX_CONTROLPARENT as u32,
            class,
            w!("小狼毫RS：外观预览"),
            WS_OVERLAPPEDWINDOW as u32,
            0,
            0,
            440,
            180,
            None,
            None,
            Some(instance),
            Some((&*state as *const Controller).cast()),
        );
        if hwnd.0.is_null() {
            return Err(windows_core::Error::from_thread().to_string());
        }
        struct Window(HWND);
        impl Drop for Window {
            fn drop(&mut self) {
                unsafe {
                    let _ = DestroyWindow(self.0);
                }
            }
        }
        let _window = Window(hwnd);
        for (slot, class, text, id) in [
            (&state.status, w!("STATIC"), w!(""), 0),
            (&state.refresh, w!("BUTTON"), w!("刷新"), REFRESH),
            (&state.close, w!("BUTTON"), w!("关闭"), CLOSE),
        ] {
            let child = CreateWindowExW(
                0,
                class,
                text,
                (WS_CHILD | WS_VISIBLE) as u32 | if id != 0 { WS_TABSTOP as u32 } else { 0 },
                0,
                0,
                1,
                1,
                Some(hwnd),
                Some(HMENU(id as *mut _)),
                Some(instance),
                None,
            );
            if child.0.is_null() {
                return Err(windows_core::Error::from_thread().to_string());
            }
            slot.set(child);
            let font = GetStockObject(DEFAULT_GUI_FONT);
            SendMessageW(child, WM_SETFONT as u32, WPARAM(font.0 as usize), LPARAM(1));
        }
        let mut monitor = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let _ = GetMonitorInfoW(
            MonitorFromWindow(hwnd, MONITOR_DEFAULTTOPRIMARY as u32),
            &mut monitor,
        );
        let dpi = GetDpiForWindow(hwnd).max(96) as i32;
        let width = 440 * dpi / 96;
        let height = 180 * dpi / 96;
        let rc = monitor.rcWork;
        // Candidate themes center their own preview. Keep the controller below
        // it so a topmost candidate window cannot cover the action buttons.
        let top = (rc.top + (rc.bottom - rc.top) / 2 + 120 * dpi / 96)
            .min(rc.bottom - height)
            .max(rc.top);
        let _ = SetWindowPos(
            hwnd,
            None,
            rc.left + (rc.right - rc.left - width) / 2,
            top,
            width,
            height,
            0,
        );
        state.layout(hwnd);
        let address = hwnd.0 as usize;
        let worker = thread::Builder::new()
            .name("preview-control".into())
            .spawn(move || worker(address, receiver, sender))
            .map_err(|e| e.to_string())?;
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetFocus(Some(state.refresh.get()));
        state.refresh();
        let mut message = MSG::default();
        loop {
            let result = GetMessageW(&mut message, None, 0, 0).0;
            if result <= 0 {
                break;
            }
            if !IsDialogMessageW(hwnd, &message).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        let _ = state.commands.send(Command::Close);
        // Normal close receives Closed only after all theme resources are gone.
        // On message-loop failure do not block the UI waiting for a stuck worker.
        if worker.is_finished() {
            let _ = worker.join();
        }
    }
    Ok(())
}
