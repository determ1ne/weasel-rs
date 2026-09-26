//! 提供 Rime 部署的原生 Windows/XAML 进度窗口，并在界面初始化失败时回退到无界面部署。
//!
//! 窗口及 XAML 对象由调用线程创建和销毁；部署工作线程通过有界邮箱回传日志与最终状态。
#![allow(unsafe_op_in_unsafe_fn)]
use crate::ui_bindings::Windows::{
    UI::{
        Color,
        ViewManagement::{UIColorType, UISettings},
        Xaml::{
            Controls::{Button, Grid, ProgressBar, ScrollViewer, TextBlock},
            ElementTheme, FrameworkElement,
            Hosting::{DesktopWindowXamlSource, WindowsXamlManager},
            IXamlSourceTransparency,
            Markup::XamlReader,
            Media::{AcrylicBackgroundSource, AcrylicBrush, SolidColorBrush},
            Visibility,
        },
    },
    Win32::*,
};
use std::{
    cell::Cell,
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};
use weasel_common::{
    comrt::WinRtApartment, deploy_protocol::DeployComplete, process::SingleInstance,
};
use windows_core::Interface;
use windows_strings::{HSTRING, w};

pub const WM_DEPLOY_FINISHED: u32 = WM_APP as u32 + 31;
use crate::deploy_job::{PREVIEW_LIMIT, UiMailbox, diagnostic};

/// 标记部署工作线程已报告完成，使窗口过程此后允许响应关闭请求。
static FINISHED: AtomicBool = AtomicBool::new(false);

/// 启动部署界面；界面初始化失败时，仅在部署锁仍由本线程持有的情况下启动无界面部署。
///
/// 界面流程正常结束时返回 `Ok(())`；锁获取、界面启动失败且无法回退，或无界面部署失败时返回错误。
pub fn run() -> Result<(), String> {
    let result = (|| {
        let guard = SingleInstance::acquire("deploy-ui-active").map_err(|e| e.to_string())?;
        with_fallback(
            guard,
            |guard| unsafe {
                let _apartment = WinRtApartment::initialize_sta().map_err(|e| e.to_string())?;
                run_initialized(guard)
            },
            |guard| {
                thread::Builder::new()
                    .name("rime-deploy-headless".into())
                    .spawn(move || crate::deploy_job::run(None, guard))
                    .map_err(|e| e.to_string())?
                    .join()
                    .map_err(|_| "deployment worker panicked".to_owned())
            },
        )
    })();
    if let Err(error) = &result {
        diagnostic(&format!("deployment UI failed: {error}"));
        crate::deploy_telemetry::Telemetry::stdout().finish_and_wait(
            weasel_common::deploy_protocol::DeployComplete {
                success: false,
                exit_code: None,
                message: error.clone(),
            },
        );
    }
    match result? {
        Some(done) if !done.success => Err(done.message),
        _ => Ok(()),
    }
}

/// 在界面初始化失败时复用部署锁启动无界面任务。
///
/// `ui` 只有在工作线程开始部署时才应从 `guard` 中取走锁；若界面返回错误且锁仍在，
/// `headless` 会被调用一次。锁已被取走时表示部署可能已经开始，此时必须原样返回界面错误，
/// 以免并发启动第二次部署。无界面任务的完成结果与初始化错误分别通过 `Some` 和 `Err` 表示。
fn with_fallback<G>(
    guard: G,
    ui: impl FnOnce(&mut Option<G>) -> Result<(), String>,
    headless: impl FnOnce(G) -> Result<DeployComplete, String>,
) -> Result<Option<DeployComplete>, String> {
    let mut guard = Some(guard);
    match ui(&mut guard) {
        Ok(()) => Ok(None),
        Err(error) => match guard {
            Some(guard) => {
                diagnostic(&format!(
                    "deployment UI unavailable; running --deploy without UI: {error}"
                ));
                headless(guard).map(Some)
            }
            None => Err(error),
        },
    }
}

/// 原生部署窗口的独占句柄；销毁时同时清理窗口定时器。
struct Window(HWND);
/// 绑定到当前线程的 Windows XAML 管理器，负责在释放时关闭线程级 XAML 环境。
struct XamlManager(WindowsXamlManager);
impl Drop for XamlManager {
    fn drop(&mut self) {
        let _ = self.0.Close();
    }
}
/// 桌面 XAML Island 的所有权包装，确保其内容和原生资源按作用域关闭。
struct XamlSource(DesktopWindowXamlSource);
impl Drop for XamlSource {
    fn drop(&mut self) {
        let _ = self.0.Close();
    }
}
impl Drop for Window {
    fn drop(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.0), 1);
            let _ = DestroyWindow(self.0);
        }
    }
}

/// 原生父窗口与 XAML 子窗口间的尺寸同步及拖动输入布局。
struct Layout {
    /// 承载 Island 的顶层窗口。
    parent: HWND,
    /// Island 创建的原生子窗口。
    child: HWND,
    /// XAML 根网格；按父窗口客户区和 DPI 更新其逻辑尺寸。
    root: Grid,
    /// 将可交互控件从窗口拖动输入区域中排除。
    drag: crate::deploy_drag::DragLayer,
    /// 上次应用的客户区宽、高和 DPI，用于跳过无变化的布局更新。
    size: Cell<(i32, i32, u32)>,
}
impl Layout {
    /// 将 Island 子窗口和根网格同步到父窗口当前客户区。
    ///
    /// 先读取客户区与 DPI；尺寸三元组未变化时不调用窗口/XAML 更新接口。
    /// Win32 或 XAML 更新失败时返回错误，调用方负责记录并继续窗口消息处理。
    unsafe fn resize(&self) -> Result<(), String> {
        let mut rect = RECT::default();
        GetClientRect(self.parent, &mut rect)
            .ok()
            .map_err(|e| e.to_string())?;
        let size = (
            rect.right,
            rect.bottom,
            GetDpiForWindow(self.parent).max(96),
        );
        if self.size.replace(size) == size {
            return Ok(());
        }
        SetWindowPos(
            self.child,
            None,
            0,
            0,
            size.0,
            size.1,
            (SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW) as u32,
        )
        .ok()
        .map_err(|e| e.to_string())?;
        let scale = size.2 as f64 / 96.0;
        self.root
            .SetWidth(size.0 as f64 / scale)
            .map_err(|e| e.to_string())?;
        self.root
            .SetHeight(size.1 as f64 / scale)
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}
/// 在销毁绑定时清空父窗口中的裸指针，避免窗口过程继续访问已释放的布局。
struct LayoutBinding(Box<Layout>);
impl Drop for LayoutBinding {
    fn drop(&mut self) {
        unsafe {
            SetWindowLongPtrW(self.0.parent, GWLP_USERDATA, 0);
        }
    }
}

/// 在已初始化的单线程 COM/XAML 环境中创建窗口、运行消息循环并等待部署工作线程结束。
///
/// `guard` 在工作线程启动前由调用线程持有；启动成功后转移给工作线程，使部署锁覆盖
/// 整个任务。XAML/窗口初始化阶段的错误会保留该锁供外层回退；启动工作线程后的错误
/// 不得触发第二次部署。函数返回前清理邮箱窗口句柄、回调绑定和窗口相关资源。
unsafe fn run_initialized(guard: &mut Option<SingleInstance>) -> Result<(), String> {
    let _ = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    let mut cursor = POINT::default();
    let _ = GetCursorPos(&mut cursor);
    let anchor = RECT {
        left: cursor.x,
        top: cursor.y,
        right: cursor.x + 1,
        bottom: cursor.y + 1,
    };
    let monitor = MonitorFromRect(&anchor, MONITOR_DEFAULTTONEAREST as u32);
    let mut monitor_info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    GetMonitorInfoW(monitor, &mut monitor_info)
        .ok()
        .map_err(|e| e.to_string())?;
    let work = monitor_info.rcWork;
    let name = w!("Weasel-RS: Rime 部署");
    let class = WNDCLASSW {
        lpfnWndProc: Some(window_proc),
        hInstance: GetModuleHandleW(None),
        hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
        lpszClassName: name,
        ..Default::default()
    };
    let _ = RegisterClassW(&class);
    let window = Window(CreateWindowExW(
        0,
        name,
        name,
        (WS_CAPTION | WS_THICKFRAME) as u32,
        work.left,
        work.top,
        720,
        480,
        None,
        None,
        Some(class.hInstance),
        None,
    ));
    if window.0.0.is_null() {
        return Err(format!(
            "创建部署窗口失败：{}",
            std::io::Error::last_os_error()
        ));
    }
    let hwnd = window.0;
    let _manager =
        XamlManager(WindowsXamlManager::InitializeForCurrentThread().map_err(|e| e.to_string())?);
    // A transparent Grid is insufficient: the Island has its own opaque backing.
    let transparency = (|| -> windows_core::Result<()> {
        let window = crate::ui_bindings::Windows::UI::Xaml::Window::Current()?;
        window
            .cast::<IXamlSourceTransparency>()?
            .SetIsBackgroundTransparent(true)
    })();
    if let Err(error) = &transparency {
        diagnostic(&format!(
            "XAML Island background transparency unavailable: {error}"
        ));
    }
    let source = XamlSource(DesktopWindowXamlSource::new().map_err(|e| e.to_string())?);
    let native: IDesktopWindowXamlSourceNative = source.0.cast().map_err(|e| e.to_string())?;
    native
        .AttachToWindow(hwnd)
        .ok()
        .map_err(|e| e.to_string())?;
    let child = native.WindowHandle().map_err(|e| e.to_string())?;
    let input: IDesktopWindowXamlSourceNative2 = source.0.cast().map_err(|e| e.to_string())?;
    let root: Grid = XamlReader::Load(&HSTRING::from(include_str!("deploy.xaml")))
        .map_err(|e| e.to_string())?
        .cast()
        .map_err(|e| e.to_string())?;
    let element: FrameworkElement = root.cast().map_err(|e| e.to_string())?;
    let log: TextBlock = element
        .FindName(&HSTRING::from("LogText"))
        .and_then(|x| x.cast())
        .map_err(|e| e.to_string())?;
    let status: TextBlock = element
        .FindName(&HSTRING::from("Status"))
        .and_then(|x| x.cast())
        .map_err(|e| e.to_string())?;
    let scroll: ScrollViewer = element
        .FindName(&HSTRING::from("LogScroll"))
        .and_then(|x| x.cast())
        .map_err(|e| e.to_string())?;
    let progress: ProgressBar = element
        .FindName(&HSTRING::from("Progress"))
        .and_then(|x| x.cast())
        .map_err(|e| e.to_string())?;
    let okay: Button = element
        .FindName(&HSTRING::from("Okay"))
        .and_then(|x| x.cast())
        .map_err(|e| e.to_string())?;
    source.0.SetContent(&root).map_err(|e| e.to_string())?;
    let layout = LayoutBinding(Box::new(Layout {
        parent: hwnd,
        child,
        root: root.clone(),
        drag: crate::deploy_drag::DragLayer::new(hwnd, &root, &scroll, &okay)?,
        size: Cell::new((-1, -1, 0)),
    }));
    SetWindowLongPtrW(hwnd, GWLP_USERDATA, &*layout.0 as *const Layout as isize);
    let close_hwnd = hwnd.0 as usize;
    let _drag_layout = root
        .LayoutUpdated(move |_, _| unsafe {
            let ptr = GetWindowLongPtrW(HWND(close_hwnd as *mut _), GWLP_USERDATA) as *const Layout;
            if let Some(layout) = ptr.as_ref() {
                if let Err(error) = layout.drag.update() {
                    diagnostic(&format!("drag layer layout failed: {error}"));
                }
            }
        })
        .map_err(|e| e.to_string())?;
    let _click = okay
        .Click(move |_, _| unsafe {
            let _ = PostMessageW(
                Some(HWND(close_hwnd as *mut _)),
                WM_CLOSE as u32,
                WPARAM(0),
                LPARAM(0),
            );
        })
        .map_err(|e| e.to_string())?;
    let dpi = GetDpiForWindow(hwnd).max(96) as i32;
    let width = 720 * dpi / 96;
    let height = 480 * dpi / 96;
    let (x, y) = centered_position(&work, width, height);
    SetWindowPos(
        hwnd,
        None,
        x,
        y,
        width,
        height,
        (SWP_NOZORDER | SWP_FRAMECHANGED) as u32,
    )
    .ok()
    .map_err(|e| e.to_string())?;
    let margins = MARGINS {
        cxLeftWidth: -1,
        cxRightWidth: -1,
        cyTopHeight: -1,
        cyBottomHeight: -1,
    };
    let extended = DwmExtendFrameIntoClientArea(hwnd, &margins);
    if let Err(error) = extended.ok() {
        diagnostic(&format!("DWM extend full frame unavailable: {error}"));
    }
    let corner = DWMWCP_ROUND;
    let rounded = DwmSetWindowAttribute(
        hwnd,
        DWMWA_WINDOW_CORNER_PREFERENCE as u32,
        &corner as *const _ as _,
        std::mem::size_of_val(&corner) as u32,
    );
    if let Err(error) = rounded.ok() {
        diagnostic(&format!("DWM rounded corners unavailable: {error}"));
    }
    let backdrop = DWMSBT_MAINWINDOW as i32;
    let backdrop_result = DwmSetWindowAttribute(
        hwnd,
        DWMWA_SYSTEMBACKDROP_TYPE as u32,
        &backdrop as *const _ as _,
        4,
    );
    if let Err(error) = backdrop_result.ok() {
        diagnostic(&format!("DWM Mica backdrop unavailable: {error}"));
    }
    let mica = backdrop_result.is_ok() && transparency.is_ok() && extended.is_ok();
    let settings = UISettings::new().map_err(|e| e.to_string())?;
    let mut theme = None;
    apply_theme(hwnd, &root, &settings, mica, &mut theme);
    layout.0.resize()?;
    let _ = ShowWindow(hwnd, SW_SHOW);
    // Bring the interactive window to the foreground
    let _ = SetForegroundWindow(hwnd);
    let _ = SetWindowPos(
        hwnd,
        Some(HWND_TOP),
        0,
        0,
        0,
        0,
        (SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW) as u32,
    );
    let mailbox = Arc::new(UiMailbox::default());
    mailbox.set_window(hwnd.0 as usize);
    let sender = mailbox.clone();
    let guard = guard.take().ok_or("deployment already started")?;
    let worker = thread::Builder::new()
        .name("rime-deploy-ui-worker".into())
        .spawn(move || crate::deploy_job::run(Some(sender), guard))
        .map_err(|e| e.to_string())?;
    let mut text = LogBuffer::default();
    let mut message = MSG::default();
    let mut scroll_pending = false;
    while GetMessageW(&mut message, None, 0, 0).0 > 0 {
        {
            // Drain on native events, not a permanent polling timer. A short
            // one-shot timer below only defers scrolling until XAML laid out text.
            if message.hwnd == hwnd && message.message == WM_TIMER as u32 && message.wParam.0 == 1 {
                let _ = KillTimer(Some(hwnd), 1);
            }
            apply_theme(hwnd, &root, &settings, mica, &mut theme);
            if scroll_pending {
                if let Ok(height) = scroll.ScrollableHeight() {
                    let _ = scroll.ChangeView(None, Some(height), None);
                }
                scroll_pending = false;
            }
            let mut dirty = false;
            let pending = mailbox.take();
            text.truncated |= pending.omitted;
            for chunk in pending.chunks {
                text.append(&chunk);
                dirty = true;
            }
            if let Some(done) = pending.complete {
                text.append(&format!("\n{}\n", done.message));
                dirty = true;
                let _ = status.SetText(&HSTRING::from(done.message));
                let _ = progress.SetIsIndeterminate(false);
                let _ = progress.SetVisibility(Visibility::Collapsed);
                let _ = okay.SetVisibility(Visibility::Visible);
                FINISHED.store(true, Ordering::Release);
            }
            if dirty {
                let _ = log.SetText(&HSTRING::from(text.text()));
                scroll_pending = true;
                let _ = SetTimer(Some(hwnd), 1, 16, None);
            }
        }
        // The input layer uses native mouse messages, not XAML input.
        if message.hwnd != layout.0.drag.hwnd()
            && input
                .PreTranslateMessage(&message)
                .unwrap_or_default()
                .as_bool()
        {
            continue;
        }
        let _ = TranslateMessage(&message);
        let _ = DispatchMessageW(&message);
    }
    // WM_CLOSE is ignored while the worker owns the deployment lock.
    let _ = worker.join();
    mailbox.set_window(0);
    drop(_drag_layout);
    drop(layout);
    // Controls drop before their source, manager and native window, including
    // initialization failures that fall back to the headless deployer.
    Ok(())
}

/// 处理部署窗口的非客户区、关闭、尺寸、DPI 与最小尺寸消息。
///
/// 部署完成前拦截关闭请求；尺寸变化时同步 XAML 子窗口布局，其他消息交由默认窗口过程。
unsafe extern "system" fn window_proc(hwnd: HWND, message: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // Keep WS_THICKFRAME for DWM shadow/rounding, but let XAML occupy the caption.
    if message == WM_NCCALCSIZE as u32 && wp.0 != 0 {
        return LRESULT(0);
    }
    if message == WM_NCHITTEST as u32 {
        return LRESULT(window_hit_test(hwnd, lp) as isize);
    }
    if message == WM_NCLBUTTONDBLCLK as u32 && wp.0 == HTCAPTION as usize {
        return LRESULT(0); // This dialog does not offer maximize.
    }
    if message == WM_CLOSE as u32
        || (message == WM_SYSCOMMAND as u32 && wp.0 & 0xfff0 == SC_CLOSE as usize)
    {
        if FINISHED.load(Ordering::Acquire) {
            PostQuitMessage(0);
        }
        return LRESULT(0);
    }
    if message == WM_SIZE as u32 || message == WM_DPICHANGED as u32 {
        if message == WM_DPICHANGED as u32 {
            let rect = &*(lp.0 as *const RECT);
            let _ = SetWindowPos(
                hwnd,
                None,
                rect.left,
                rect.top,
                rect.right - rect.left,
                rect.bottom - rect.top,
                SWP_NOZORDER as u32,
            );
        }
        let layout = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Layout;
        if let Some(layout) = layout.as_ref() {
            if let Err(error) = layout.resize() {
                diagnostic(&format!("child layout failed: {error}"));
            }
        }
        return LRESULT(0);
    }
    if message == WM_GETMINMAXINFO as u32 {
        let limits = &mut *(lp.0 as *mut MINMAXINFO);
        let dpi = GetDpiForWindow(hwnd).max(96) as i32;
        limits.ptMinTrackSize.x = 560 * dpi / 96;
        limits.ptMinTrackSize.y = 360 * dpi / 96;
        return LRESULT(0);
    }
    DefWindowProcW(hwnd, message, wp, lp)
}

/// 将屏幕坐标下的非客户区命中测试转换为与窗口 DPI 无关的逻辑坐标后判定边框区域。
///
/// 无法读取窗口矩形时返回客户区命中，避免因几何信息不可用而意外启动缩放操作。
unsafe fn window_hit_test(hwnd: HWND, lp: LPARAM) -> i32 {
    let mut rect = RECT::default();
    if !GetWindowRect(hwnd, &mut rect).as_bool() {
        return HTCLIENT;
    }
    let x = lp.0 as u16 as i16 as i32 - rect.left;
    let y = (lp.0 >> 16) as u16 as i16 as i32 - rect.top;
    let scale = GetDpiForWindow(hwnd).max(96) as f64 / 96.0;
    frame_hit_test(
        x as f64 / scale,
        y as f64 / scale,
        (rect.right - rect.left) as f64 / scale,
        (rect.bottom - rect.top) as f64 / scale,
    )
}

/// 在指定工作区内居中放置给定像素尺寸的窗口。
fn centered_position(work: &RECT, width: i32, height: i32) -> (i32, i32) {
    (
        work.left + (work.right - work.left - width) / 2,
        work.top + (work.bottom - work.top - height) / 2,
    )
}

/// 按逻辑坐标区分窗口缩放边缘、标题栏拖动区和普通客户区。
///
/// 超出窗口范围的点按客户区处理；边缘判定优先于标题栏，以保留四边和四角缩放。
fn frame_hit_test(x: f64, y: f64, width: f64, height: f64) -> i32 {
    if x < 0.0 || y < 0.0 || x >= width || y >= height {
        return HTCLIENT;
    }
    let (left, right, top, bottom) = (x < 8.0, x >= width - 8.0, y < 8.0, y >= height - 8.0);
    match (left, right, top, bottom) {
        (true, _, true, _) => HTTOPLEFT,
        (_, true, true, _) => HTTOPRIGHT,
        (true, _, _, true) => HTBOTTOMLEFT,
        (_, true, _, true) => HTBOTTOMRIGHT,
        (true, _, _, _) => HTLEFT,
        (_, true, _, _) => HTRIGHT,
        (_, _, true, _) => HTTOP,
        (_, _, _, true) => HTBOTTOM,
        _ if y < 56.0 => HTCAPTION,
        _ => HTCLIENT,
    }
}

/// 根据系统背景色同步 XAML 与 DWM 的深浅主题，并选择 Mica、亚克力或纯色背景。
///
/// `previous` 用于跳过未变化主题的重复设置；DWM/画刷能力不可用时尽量降级，诊断输出
/// 失败也不会中断窗口运行。
fn apply_theme(
    hwnd: HWND,
    root: &Grid,
    settings: &UISettings,
    mica: bool,
    previous: &mut Option<bool>,
) {
    let dark = settings
        .GetColorValue(UIColorType::Background)
        .map(|c| c.R < 128)
        .unwrap_or(true);
    if *previous == Some(dark) {
        return;
    }
    *previous = Some(dark);
    let _ = root.SetRequestedTheme(if dark {
        ElementTheme::Dark
    } else {
        ElementTheme::Light
    });
    let dark_value = dark as i32;
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
            &dark_value as *const _ as _,
            4,
        );
    }
    let tint = if dark {
        Color {
            A: 255,
            R: 32,
            G: 32,
            B: 32,
        }
    } else {
        Color {
            A: 255,
            R: 243,
            G: 243,
            B: 243,
        }
    };
    if mica {
        return;
    }
    let acrylic = (|| -> windows_core::Result<()> {
        let brush = AcrylicBrush::new()?;
        brush.SetBackgroundSource(AcrylicBackgroundSource::HostBackdrop)?;
        brush.SetTintColor(tint)?;
        brush.SetTintOpacity(0.8)?;
        brush.SetFallbackColor(tint)?;
        root.SetBackground(&brush)
    })();
    if let Err(error) = acrylic {
        // The broker may have detached and closed stderr after completion.
        // Material changes must not panic on a broken diagnostic pipe.
        use std::io::Write;
        let _ = writeln!(
            std::io::stderr(),
            "weasel-server: acrylic unavailable: {error}"
        );
        if let Ok(brush) = SolidColorBrush::CreateInstanceWithColor(tint) {
            let _ = root.SetBackground(&brush);
        }
    }
}

#[derive(Default)]
/// 为界面预览保留有界日志尾部，避免部署输出无限增长占用内存。
struct LogBuffer {
    /// 按换行切分并保留的日志片段。
    chunks: VecDeque<String>,
    /// 当前片段的 UTF-8 字节总数。
    bytes: usize,
    /// 是否因单行过长或总容量限制而省略过旧/过长内容。
    truncated: bool,
}
impl LogBuffer {
    /// 追加日志并按字节上限及片段数量上限裁剪；截断位置会向前移至 UTF-8 字符边界。
    fn append(&mut self, text: &str) {
        // Split at line boundaries; a pathological single line is also bounded.
        for line in text.split_inclusive('\n') {
            let mut start = line.len().saturating_sub(PREVIEW_LIMIT);
            while !line.is_char_boundary(start) {
                start += 1;
            }
            if start != 0 {
                self.truncated = true;
            }
            let line = line[start..].to_owned();
            self.bytes += line.len();
            self.chunks.push_back(line);
            while self.bytes > PREVIEW_LIMIT || self.chunks.len() > 10000 {
                self.bytes -= self.chunks.pop_front().unwrap().len();
                self.truncated = true;
            }
        }
    }
    /// 组合当前预览文本；存在截断时在保留的日志前提示完整日志另有保存。
    fn text(&self) -> String {
        let mut text = if self.truncated {
            "[较早日志已省略；完整日志已保存]\n".into()
        } else {
            String::new()
        };
        for chunk in &self.chunks {
            text.push_str(chunk);
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn initialization_failure_falls_back_once_with_the_same_guard() {
        let done = DeployComplete {
            success: true,
            exit_code: Some(0),
            message: "done".into(),
        };
        let mut calls = 0;
        let result = with_fallback(
            42,
            |_| Err("XAML unavailable".into()),
            |guard| {
                assert_eq!(guard, 42);
                calls += 1;
                Ok(done.clone())
            },
        )
        .unwrap();
        assert_eq!(calls, 1);
        assert_eq!(result, Some(done));
    }

    #[test]
    fn successful_ui_and_started_workers_never_start_a_second_deployment() {
        assert!(
            with_fallback((), |_| Ok(()), |_| panic!("unexpected fallback"))
                .unwrap()
                .is_none()
        );
        let result = with_fallback(
            (),
            |guard| {
                guard.take();
                Err("worker already started".into())
            },
            |_| panic!("deployment must not run twice"),
        );
        assert_eq!(result.unwrap_err(), "worker already started");
    }

    #[test]
    fn headless_deployment_failure_remains_a_completion_not_an_initialization_error() {
        let done = DeployComplete {
            success: false,
            exit_code: Some(1),
            message: "deploy failed".into(),
        };
        let result = with_fallback((), |_| Err("no UI".into()), |_| Ok(done.clone())).unwrap();
        assert_eq!(result, Some(done));
        assert_eq!(
            with_fallback((), |_| Err("no UI".into()), |_| Err("spawn failed".into())).unwrap_err(),
            "spawn failed"
        );
    }
    #[test]
    fn startup_center_uses_work_area_and_negative_monitor_coordinates() {
        let work = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1040,
        };
        assert_eq!(centered_position(&work, 720, 480), (600, 280));
        let secondary = RECT {
            left: -2560,
            top: -200,
            right: 0,
            bottom: 1200,
        };
        assert_eq!(centered_position(&secondary, 1080, 720), (-1820, 140));
    }
    #[test]
    fn custom_frame_preserves_caption_resize_and_client_regions() {
        for (x, y, expected) in [
            (1.0, 1.0, HTTOPLEFT),
            (719.0, 1.0, HTTOPRIGHT),
            (1.0, 479.0, HTBOTTOMLEFT),
            (719.0, 479.0, HTBOTTOMRIGHT),
            (360.0, 1.0, HTTOP),
            (360.0, 479.0, HTBOTTOM),
            (1.0, 240.0, HTLEFT),
            (719.0, 240.0, HTRIGHT),
            (360.0, 30.0, HTCAPTION),
            (360.0, 56.0, HTCLIENT),
            (360.0, 240.0, HTCLIENT),
            (660.0, 440.0, HTCLIENT),
            (-1.0, 30.0, HTCLIENT),
        ] {
            assert_eq!(frame_hit_test(x, y, 720.0, 480.0), expected);
        }
    }
    #[test]
    fn log_buffer_is_bounded_and_unicode_safe() {
        let mut log = LogBuffer::default();
        log.append(&"中".repeat(800000));
        assert!(log.bytes <= PREVIEW_LIMIT);
        assert!(log.text().starts_with("[较早"));
        log.append(&"x\n".repeat(10001));
        assert!(log.chunks.len() <= 10000);
    }
}
