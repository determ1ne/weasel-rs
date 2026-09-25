//! Optional WinSparkle integration. A missing DLL or unconfigured feed never blocks input.
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

static TRAY_WINDOW: AtomicIsize = AtomicIsize::new(0);

unsafe extern "C" fn can_shutdown() -> i32 {
    i32::from(crate::windows_tray::can_shutdown_for_update())
}

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

pub struct Updater {
    // The library must stay loaded until cleanup returns and the function pointers are unused.
    _library: Library,
    cleanup: Action,
    check_with_ui: Action,
}

impl Updater {
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

    pub fn check_with_ui(&self) {
        unsafe { (self.check_with_ui)() };
    }
}

impl Drop for Updater {
    fn drop(&mut self) {
        TRAY_WINDOW.store(0, Ordering::Release);
        unsafe { (self.cleanup)() };
    }
}
