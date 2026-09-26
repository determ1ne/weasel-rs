//! 原生外观预览控制器。

use crate::{d2d_bindings::*, theme_api::UiMode, ui_runtime::UiHandle};
use std::{cell::Cell, sync::mpsc, thread};
use windows_strings::{HSTRING, w};

const COMPLETE: u32 = WM_APP as u32 + 40;
const REFRESH: usize = 1;
const CLOSE: usize = 2;

/// 由窗口线程发送给主题工作线程的串行命令。
enum Command {
    /// 重新读取配置并尝试加载当前指定的主题。
    Refresh,
    /// 停止接收后续刷新，并释放工作线程持有的主题资源。
    Close,
}

/// 工作线程发回窗口线程的操作结果。
enum Completion {
    /// 一次刷新结束；错误不会替换当前正在显示的主题。
    Refreshed(Result<String, String>),
    /// 工作线程已退出，且其主题资源已释放。
    Closed,
}

/// 窗口线程拥有的控制状态；跨线程通信只通过命令和完成通道进行。
///
/// `Cell` 中的窗口句柄及状态仅由创建窗口的 UI 线程访问。`busy` 防止同时排队多个
/// 刷新，`closing` 则阻止关闭流程开始后再接受新的刷新请求。
struct Controller {
    /// 向唯一工作线程发送命令；接收端断开表示工作线程已不可用。
    commands: mpsc::Sender<Command>,
    /// 接收工作线程结果，由窗口过程在完成消息中排空。
    results: mpsc::Receiver<Completion>,
    /// 刷新按钮的窗口句柄。
    refresh: Cell<HWND>,
    /// 关闭按钮的窗口句柄。
    close: Cell<HWND>,
    /// 显示加载状态或当前主题名称的静态文本句柄。
    status: Cell<HWND>,
    /// 是否已有刷新命令正在执行或排队。
    busy: Cell<bool>,
    /// 是否已进入关闭流程。
    closing: Cell<bool>,
}

/// 在专用线程中串行处理主题加载，并在资源释放及操作完成时通知窗口线程。
///
/// 每次刷新先构造新的 `UiHandle`，仅在构造成功后替换旧句柄，因此失败不会破坏现有
/// 预览。线程入口捕获 panic，避免异常越过线程边界；RPC、配置或主题初始化错误通过
/// 完成通道返回。关闭时先退出命令循环并丢弃当前句柄，再发送 `Closed`。
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
                let requested = settings.required::<String>(".theme")?;
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
    /// 若控制器仍开放且当前空闲，则排入一次刷新并立即更新界面状态。
    ///
    /// 重复点击会被忽略；命令通道断开时向消息循环投递非零退出码。
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
    /// 开始关闭流程，禁用操作按钮并通知工作线程停止。
    ///
    /// 重复关闭请求无效。实际窗口循环在收到工作线程的 `Closed` 完成项后退出。
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
    /// 按当前窗口 DPI 调整状态文本和操作按钮的位置。
    ///
    /// 子窗口尚未创建时对应句柄为空，此时跳过该控件；尺寸至少保持一个像素。
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

/// 原生窗口消息入口；只在 UI 线程操作控件，并通过通道与工作线程协调。
///
/// `WM_NCCREATE` 将控制器指针存入窗口用户数据，窗口销毁时清除。完成消息会排空结果
/// 队列，因为工作线程可能在 UI 线程处理消息前连续发送多个结果。panic 被捕获并转为
/// 消息循环失败，避免 unwind 穿过系统 ABI 边界。
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

/// 创建并运行独立的预览控制窗口，直到用户关闭或消息循环结束。
///
/// 初次刷新在消息循环启动后异步执行。窗口线程负责所有窗口操作，唯一工作线程负责
/// 配置/RPC 和主题资源；正常关闭会等待 `Closed` 消息确认资源已释放。若消息循环异常
/// 结束，仅当工作线程已退出时才等待并回收线程句柄，避免 UI 线程被卡住。
///
/// # 错误
///
/// 窗口类注册、窗口或子控件创建、工作线程创建失败时返回系统或线程错误文本。
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
            /// 在所有退出路径上销毁原生窗口。
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
