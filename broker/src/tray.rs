//! Windows 通知区域图标、隐藏窗口和菜单消息循环。

use std::{
    cell::RefCell,
    path::Path,
    sync::atomic::{AtomicU32, Ordering},
};

use crate::{bindings::*, lifecycle::Operation, operations, service_supervisor};
use weasel_common::{command_menu, logging::ComponentLogger};
use windows_strings::{HSTRING, PCWSTR, w};

/// 托盘图标向隐藏窗口发送回调的私有消息号。
const TRAY_CALLBACK_MESSAGE: u32 = WM_APP as u32 + 1;
/// 后台操作向消息线程报告“结果可读取”的私有消息号。
const OPERATION_COMPLETE: u32 = WM_APP as u32 + 2;
/// Explorer 重建任务栏后广播的注册消息号。
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

thread_local! {
    /// 更新器由创建托盘窗口的线程持有。
    static UPDATER: RefCell<Option<crate::updater::Updater>> = const { RefCell::new(None) };
}

/// 隐藏窗口及其通知区域图标数据。
pub(crate) struct TrayIcon {
    window: HWND,
    data: NOTIFYICONDATAW,
}

impl TrayIcon {
    /// 返回托盘消息窗口，供更新器注册回调。
    pub(crate) fn window(&self) -> HWND {
        self.window
    }
}

impl Drop for TrayIcon {
    fn drop(&mut self) {
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE as u32, &self.data);
            let _ = DestroyWindow(self.window);
        }
    }
}

/// 注册隐藏窗口并向通知区域添加 broker 图标。
pub(crate) fn create() -> Result<TrayIcon, Box<dyn std::error::Error>> {
    let taskbar = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
    if taskbar == 0 {
        return Err("RegisterWindowMessageW(TaskbarCreated) failed".into());
    }
    TASKBAR_CREATED.store(taskbar, Ordering::Release);
    let class_name = HSTRING::from(command_menu::BROKER_WINDOW_CLASS);
    let title = w!("weasel-rs");
    let hinstance = unsafe { GetModuleHandleW(None) };
    let window_class = WNDCLASSW {
        lpfnWndProc: Some(window_proc),
        hInstance: hinstance,
        lpszClassName: PCWSTR(class_name.as_ptr()),
        ..Default::default()
    };
    unsafe {
        let _ = RegisterClassW(&window_class);
    }
    let window = unsafe {
        CreateWindowExW(
            Default::default(),
            PCWSTR(class_name.as_ptr()),
            title,
            Default::default(),
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinstance),
            None,
        )
    };
    if window.0.is_null() {
        return Err("CreateWindowExW failed".into());
    }
    match add_icon(window) {
        Ok(data) => Ok(TrayIcon { window, data }),
        Err(error) => {
            unsafe {
                let _ = DestroyWindow(window);
            }
            Err(error)
        }
    }
}

/// 在托盘线程中启动更新器。不可用时只记录警告，不影响 broker 运行。
pub(crate) fn start_updater(directory: &Path, tray: &TrayIcon, logger: &ComponentLogger) {
    match crate::updater::Updater::start(directory, tray.window()) {
        Ok(updater) => UPDATER.with(|cell| *cell.borrow_mut() = Some(updater)),
        Err(error) => logger.record(
            weasel_common::logging::Level::WARN,
            "weasel-broker",
            format_args!("update checks unavailable: {error}"),
        ),
    }
}

/// 在退出消息循环后释放更新器。
pub(crate) fn stop_updater() {
    UPDATER.with(|cell| *cell.borrow_mut() = None);
}

/// 在当前线程分派窗口消息，直到收到退出消息或发生错误。
pub(crate) fn message_loop() {
    let mut message = MSG::default();
    unsafe {
        loop {
            let result = GetMessageW(&mut message, None, 0, 0).0;
            if result <= 0 {
                if result < 0 {
                    eprintln!(
                        "weasel-broker: GetMessageW failed: {}",
                        std::io::Error::last_os_error()
                    );
                }
                break;
            }
            let _ = TranslateMessage(&message);
            let _ = DispatchMessageW(&message);
        }
    }
}

/// 向 Windows 通知区域注册图标及回调消息。
fn add_icon(window: HWND) -> Result<NOTIFYICONDATAW, Box<dyn std::error::Error>> {
    let hinstance = unsafe { GetModuleHandleW(None) };
    let icon = unsafe { LoadIconW(Some(hinstance), w!("WEASEL_ICON")) };
    let mut data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: window,
        uID: 1,
        uFlags: (NIF_MESSAGE | NIF_ICON | NIF_TIP) as u32,
        uCallbackMessage: TRAY_CALLBACK_MESSAGE,
        hIcon: icon,
        ..Default::default()
    };
    let tip = HSTRING::from("Weasel-RS 服务器");
    let length = tip.len().min(data.szTip.len() - 1);
    data.szTip[..length].copy_from_slice(&tip[..length]);
    if !unsafe { Shell_NotifyIconW(NIM_ADD as u32, &data).as_bool() } {
        return Err("Shell_NotifyIconW(NIM_ADD) failed".into());
    }
    Ok(data)
}

/// 隐藏窗口的系统回调。
unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message != 0 && message == TASKBAR_CREATED.load(Ordering::Acquire) {
        if let Err(error) = add_icon(window) {
            eprintln!("weasel-broker: could not restore tray after Explorer restart: {error}");
        }
        return LRESULT(0);
    }
    match message {
        OPERATION_COMPLETE => show_operation_result(window),
        TRAY_CALLBACK_MESSAGE if lparam.0 as u32 == WM_RBUTTONUP as u32 => show_menu(window),
        message if message == WM_COMMAND as u32 => {
            handle_command(window, (wparam.0 & 0xffff) as u32)
        }
        message if message == WM_DESTROY as u32 => unsafe { PostQuitMessage(0) },
        _ => return unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
    LRESULT(0)
}

/// 在光标处显示托盘菜单，并把选择结果投递为窗口消息。
fn show_menu(window: HWND) {
    if operations::has_result() {
        show_operation_result(window);
        return;
    }
    let menu = unsafe { CreatePopupMenu() };
    let busy = operations::is_busy();
    unsafe {
        for (id, label) in command_menu::items(weasel_common::about::shift_pressed()) {
            let disabled = busy && matches!(id, command_menu::DEPLOY | command_menu::RESTART);
            let flags = if id == 0 {
                MF_SEPARATOR as u32
            } else {
                MF_STRING as u32
            } | if disabled { MF_GRAYED as u32 } else { 0 };
            let label = HSTRING::from(label);
            let _ = AppendMenuW(menu, flags, id as usize, PCWSTR(label.as_ptr()));
        }
        let _ = SetForegroundWindow(window);
        let mut point = POINT::default();
        let _ = GetCursorPos(&mut point);
        let command = TrackPopupMenu(
            menu,
            (TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY) as u32,
            point.x,
            point.y,
            Some(0),
            window,
            None,
        );
        let _ = DestroyMenu(menu);
        let _ = PostMessageW(Some(window), WM_NULL as u32, WPARAM(0), LPARAM(0));
        if command.0 != 0 {
            let _ = PostMessageW(
                Some(window),
                WM_COMMAND as u32,
                WPARAM(command.0 as usize),
                LPARAM(0),
            );
        }
    }
}

/// 分派托盘菜单命令。
fn handle_command(window: HWND, command: u32) {
    match command {
        command_menu::ABOUT => {
            weasel_common::about::show(&weasel_common::about::information("broker"));
        }
        command_menu::DIAGNOSTICS => show_diagnostics(),
        command_menu::DEPLOY => begin_operation(window, Operation::Deploy),
        command_menu::RESTART => begin_operation(window, Operation::Restart),
        command_menu::CHECK_UPDATES => UPDATER.with(|cell| {
            if let Some(updater) = cell.borrow().as_ref() {
                updater.check_with_ui();
            } else {
                unsafe {
                    let _ = MessageBoxW(
                        Some(window),
                        &HSTRING::from("更新功能尚未配置，或 WinSparkle.dll 不可用。"),
                        &HSTRING::from("小狼毫RS"),
                        (MB_OK | MB_ICONERROR | MB_SETFOREGROUND) as u32,
                    );
                }
            }
        }),
        command_menu::EXIT => {
            service_supervisor::request_stop();
            unsafe { PostQuitMessage(0) };
        }
        id if command_menu::is_command(id) => crate::menu_actions::open(id),
        _ => {}
    }
}

/// 启动后台操作，并把完成通知转换为窗口消息。
fn begin_operation(window: HWND, operation: Operation) {
    let raw_window = window.0 as usize;
    operations::begin(operation, move || {
        post_operation_complete(HWND(raw_window as *mut _));
    });
}

/// 将后台操作完成通知投递到托盘消息线程。
fn post_operation_complete(window: HWND) {
    if !unsafe { PostMessageW(Some(window), OPERATION_COMPLETE, WPARAM(0), LPARAM(0)) }.as_bool() {
        eprintln!(
            "weasel-broker: could not post operation completion: {}",
            std::io::Error::last_os_error()
        );
    }
}

/// 取出并显示后台操作结果；成功结果无需弹窗。
fn show_operation_result(window: HWND) {
    let Some(result) = operations::take_result() else {
        return;
    };
    if result.message.is_empty() {
        return;
    }
    let text = HSTRING::from(result.message);
    let icon = if result.failed {
        MB_ICONERROR
    } else {
        MB_ICONINFORMATION
    };
    unsafe {
        let shown = MessageBoxW(
            Some(window),
            PCWSTR(text.as_ptr()),
            w!("weasel-rs"),
            (MB_OK | MB_SETFOREGROUND | icon) as u32,
        );
        if shown == 0 {
            eprintln!(
                "weasel-broker: MessageBoxW failed: {}",
                std::io::Error::last_os_error()
            );
        }
    }
}

/// 显示 broker 和受管服务的诊断摘要。
fn show_diagnostics() {
    let mut info = weasel_common::about::information("broker");
    info.push_str(&format!(
        "\n\n正在部署/重启：{}\n正在退出：{}\n{}",
        operations::is_busy(),
        service_supervisor::is_stopping(),
        service_supervisor::diagnostic_summary()
    ));
    info.push_str(
        "\n\nTIP 故障请在对应宿主的语言栏使用 Shift＋右键 → 诊断信息。\nCtrl+C 可复制此对话框。",
    );
    weasel_common::about::show(&info);
}
