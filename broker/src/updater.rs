//! 可选的 WinSparkle 更新集成；缺少 DLL 或更新源未配置时不会阻断输入。
//!
//! 更新库按需从运行目录加载，函数指针仅在库仍加载期间有效。WinSparkle 的关闭
//! 回调可能运行于工作线程，因此通过托盘窗口投递退出命令，而不直接操作 UI 状态。
use std::{
    ffi::CString,
    path::Path,
    sync::atomic::{AtomicIsize, Ordering},
};

use libloading::Library;

use crate::bindings::{HWND, LPARAM, PostMessageW, WM_COMMAND, WPARAM};
use weasel_common::command_menu;

type SetAppDetails = unsafe extern "C" fn(*const u16, *const u16, *const u16);
type SetAppcastUrl = unsafe extern "C" fn(*const i8);
type SetPublicKey = unsafe extern "C" fn(*const i8) -> i32;
type SetShutdownRequest = unsafe extern "C" fn(unsafe extern "C" fn());
type SetCanShutdown = unsafe extern "C" fn(unsafe extern "C" fn() -> i32);
type Action = unsafe extern "C" fn();

/// 当前托盘窗口句柄的跨线程副本；零值表示尚未启用或已清理更新器。
static TRAY_WINDOW: AtomicIsize = AtomicIsize::new(0);

/// 告知 WinSparkle 托盘当前是否允许为更新而关闭。
///
/// 回调遵循 WinSparkle 的 C ABI，返回值由托盘生命周期策略转换为整数。
unsafe extern "C" fn can_shutdown() -> i32 {
    i32::from(crate::runtime::can_shutdown_for_update())
}

/// 响应 WinSparkle 的关闭请求，并将退出命令投递给托盘窗口线程。
///
/// 此回调可能从 WinSparkle 工作线程调用；窗口句柄为零时不执行操作，投递失败
/// 也不会跨线程直接销毁托盘对象。
unsafe extern "C" fn request_shutdown() {
    let window = TRAY_WINDOW.load(Ordering::Acquire);
    if window != 0 {
        // WinSparkle calls this from a worker thread after launching the installer.
        let _ = unsafe {
            PostMessageW(
                Some(HWND(window as *mut _)),
                WM_COMMAND as u32,
                WPARAM(command_menu::EXIT as usize),
                LPARAM(0),
            )
        };
    }
}

/// 持有已加载的 WinSparkle 库及其入口函数，确保函数指针不会悬空。
pub struct Updater {
    /// 必须至少保持加载到清理函数返回且所有函数指针均不再使用。
    _library: Library,
    /// 停止 WinSparkle 并释放其内部资源的入口函数。
    cleanup: Action,
    /// 打开用户可见更新检查界面的入口函数。
    check_with_ui: Action,
}

impl Updater {
    /// 从指定目录加载并初始化 WinSparkle 更新器。
    ///
    /// 仅接受编译时配置的 HTTPS appcast 地址和非空公钥。加载、符号解析或公钥
    /// 校验失败时返回错误；成功后注册关闭策略并保存托盘窗口句柄，实例销毁时清理。
    pub fn start(directory: &Path, tray_window: HWND) -> Result<Self, String> {
        let url = option_env!("WINSPARKLE_APPCAST_URL")
            .filter(|value| value.starts_with("https://") && value.ends_with("/appcast.xml"))
            .ok_or("WINSPARKLE_APPCAST_URL 未配置为 HTTPS appcast 地址")?;
        let key = option_env!("WINSPARKLE_PUBLIC_KEY")
            .filter(|value| !value.is_empty())
            .ok_or("WINSPARKLE_PUBLIC_KEY 未配置")?;
        let url = CString::new(url).map_err(|error| error.to_string())?;
        let key = CString::new(key).map_err(|error| error.to_string())?;
        let library = unsafe { Library::new(directory.join("WinSparkle.dll")) }
            .map_err(|error| format!("无法载入 WinSparkle.dll：{error}"))?;
        unsafe {
            let details: SetAppDetails = *library
                .get(b"win_sparkle_set_app_details\0")
                .map_err(|error| error.to_string())?;
            let set_url: SetAppcastUrl = *library
                .get(b"win_sparkle_set_appcast_url\0")
                .map_err(|error| error.to_string())?;
            let set_key: SetPublicKey = *library
                .get(b"win_sparkle_set_eddsa_public_key\0")
                .map_err(|error| error.to_string())?;
            let set_can_shutdown: SetCanShutdown = *library
                .get(b"win_sparkle_set_can_shutdown_callback\0")
                .map_err(|error| error.to_string())?;
            let set_shutdown: SetShutdownRequest = *library
                .get(b"win_sparkle_set_shutdown_request_callback\0")
                .map_err(|error| error.to_string())?;
            let init: Action = *library
                .get(b"win_sparkle_init\0")
                .map_err(|error| error.to_string())?;
            let cleanup: Action = *library
                .get(b"win_sparkle_cleanup\0")
                .map_err(|error| error.to_string())?;
            let check_with_ui: Action = *library
                .get(b"win_sparkle_check_update_with_ui\0")
                .map_err(|error| error.to_string())?;

            let wide = |value: &str| value.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
            details(
                wide("Weasel-RS").as_ptr(),
                wide("小狼毫RS").as_ptr(),
                wide(env!("CARGO_PKG_VERSION")).as_ptr(),
            );
            set_url(url.as_ptr());
            if set_key(key.as_ptr()) != 1 {
                return Err("WinSparkle 拒绝 EdDSA 公钥".to_owned());
            }
            set_can_shutdown(can_shutdown);
            set_shutdown(request_shutdown);
            TRAY_WINDOW.store(tray_window.0 as isize, Ordering::Release);
            init();
            Ok(Self {
                _library: library,
                cleanup,
                check_with_ui,
            })
        }
    }

    /// 请求 WinSparkle 显示交互式更新检查界面。
    pub fn check_with_ui(&self) {
        unsafe { (self.check_with_ui)() };
    }
}

/// 清除回调可见的窗口句柄，并在库仍加载时清理 WinSparkle。
impl Drop for Updater {
    fn drop(&mut self) {
        TRAY_WINDOW.store(0, Ordering::Release);
        unsafe { (self.cleanup)() };
    }
}
