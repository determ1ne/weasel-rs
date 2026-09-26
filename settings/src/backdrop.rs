//! Slint 可能在事件循环开始后才创建 HWND，必须等待原生窗口就绪。
//! 不将 show() 返回后暂时没有 HWND 误判为材质失败；无需轮询或常驻 Hook。
use crate::{SettingsWindow, bindings::*};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use slint::{ComponentHandle, winit_030::WinitWindowAccessor};

/// 当前系统是否支持主窗口系统材质所需的 Windows 版本。
pub fn supported() -> bool {
    windows_version::OsVersion::current() >= windows_version::OsVersion::new(10, 0, 0, 22621)
}
/// 在 Slint 事件循环中等待原生窗口句柄并异步应用 Mica 材质。
///
/// 任务仅持有弱窗口句柄；窗口销毁或调度、窗口句柄、DWM 初始化失败时关闭
/// Slint 的材质标志并记录错误，不阻塞调用线程。
pub fn apply(ui: &SettingsWindow) {
    if !supported() {
        return;
    }
    let weak = ui.as_weak();
    if let Err(error) = slint::spawn_local(async move {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let result = match ui.window().winit_window().await {
            Ok(window) => configure(&window),
            Err(error) => Err(error.to_string()),
        };
        match result {
            Ok(()) => ui.set_mica_enabled(true),
            Err(error) => {
                ui.set_mica_enabled(false);
                eprintln!("weasel-settings: Mica unavailable: {error}");
            }
        }
    }) {
        ui.set_mica_enabled(false);
        eprintln!("weasel-settings: Mica initialization scheduling failed: {error}");
    }
}

/// 将 DWM 扩展帧和系统背景材质应用到 Win32 窗口，再安装自定义框架处理。
///
/// 非 Win32 句柄或 DWM 调用失败会返回错误；自定义框架安装失败只记录日志，材质
/// 配置仍视为成功，因为它不影响已完成的 DWM 设置。
fn configure(window: &slint::winit_030::winit::window::Window) -> Result<(), String> {
    let handle = window.window_handle().map_err(|e| e.to_string())?;
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return Err("非 Win32 窗口".into());
    };
    let hwnd = HWND(handle.hwnd.get() as *mut core::ffi::c_void);
    unsafe {
        // 深浅色由 Winit 跟随系统更新，Slint 使用默认系统调色板。
        // 原生 DWM 标题栏按钮需要整个客户区属于扩展帧。
        let margins = MARGINS {
            cxLeftWidth: -1,
            cxRightWidth: -1,
            cyTopHeight: -1,
            cyBottomHeight: -1,
        };
        DwmExtendFrameIntoClientArea(hwnd, &margins)
            .ok()
            .map_err(|e| e.to_string())?;
        let backdrop = DWMSBT_MAINWINDOW as i32;
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE as u32,
            &backdrop as *const _ as _,
            core::mem::size_of_val(&backdrop) as u32,
        )
        .ok()
        .map_err(|e| e.to_string())?;
    }
    // 扩展帧和材质就绪后再触发 SWP_FRAMECHANGED，让 DWM 重算按钮布局。
    if let Err(error) = crate::frame::install(hwnd) {
        eprintln!("weasel-settings: custom frame unavailable: {error}");
    }
    Ok(())
}
