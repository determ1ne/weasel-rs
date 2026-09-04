//! Dispatch RPC notifications on the TSF apartment, never on the pipe thread.
use crate::bindings::*;
use std::{cell::Cell, rc::Rc};
use windows_core::{Error, Result};
use windows_strings::{HSTRING, PCWSTR};

pub const UPDATE_MESSAGE: u32 = WM_APP as u32 + 23;
const EMOJI_MESSAGE: u32 = UPDATE_MESSAGE + 1;
const THEME_MESSAGE: u32 = UPDATE_MESSAGE + 2;

struct Callback {
    invoke: Rc<dyn Fn()>,
    theme_pending: Cell<bool>,
}

pub struct UpdateWindow {
    pub hwnd: HWND,
    // Stable allocation referenced by GWLP_USERDATA until the window is destroyed.
    callback: Box<Callback>,
    class_name: HSTRING,
}

impl UpdateWindow {
    // Queue the shortcut after EndComposition succeeds, allowing the edit
    // session's write lock to be released before shell UI receives input.
    pub fn open_emoji_after_edit(&self) {
        if !unsafe { PostMessageW(Some(self.hwnd), EMOJI_MESSAGE, WPARAM(0), LPARAM(0)) }.as_bool()
        {
            let _ = std::io::Write::write_fmt(
                &mut std::io::stderr(),
                format_args!(
                    "weasel-tip: could not queue emoji panel: {}",
                    std::io::Error::last_os_error()
                ),
            );
        }
    }

    pub fn new(callback: impl Fn() + 'static) -> Result<Self> {
        let mut callback = Box::new(Callback {
            invoke: Rc::new(callback),
            theme_pending: Cell::new(false),
        });
        let name = HSTRING::from(format!("weasel-rs-tip-updates-{:p}", &*callback));
        unsafe {
            let instance = GetModuleHandleW(None);
            let class = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: instance,
                lpszClassName: PCWSTR(name.as_ptr()),
                ..Default::default()
            };
            if RegisterClassW(&class).0 == 0 {
                return Err(Error::from_thread());
            }
            let hwnd = CreateWindowExW(
                (WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE) as u32,
                PCWSTR(name.as_ptr()),
                PCWSTR(name.as_ptr()),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance),
                None,
            );
            if hwnd.0.is_null() {
                let error = Error::from_thread();
                let _ = UnregisterClassW(PCWSTR(name.as_ptr()), Some(instance));
                return Err(error);
            }
            SetWindowLongPtrW(
                hwnd,
                GWLP_USERDATA,
                // x86 aliases this API to SetWindowLongW (i32); x64 uses isize.
                (&mut *callback as *mut Callback) as _,
            );
            Ok(Self {
                hwnd,
                callback,
                class_name: name,
            })
        }
    }
}

impl Drop for UpdateWindow {
    fn drop(&mut self) {
        unsafe {
            SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
            let _ = DestroyWindow(self.hwnd);
            let _ = UnregisterClassW(
                PCWSTR(self.class_name.as_ptr()),
                Some(GetModuleHandleW(None)),
            );
        }
        let _ = &self.callback;
    }
}

unsafe extern "system" fn window_proc(hwnd: HWND, message: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    crate::boundary::guard(None, || Ok(unsafe { dispatch(hwnd, message, wp, lp) }))
        .unwrap_or(LRESULT(0))
}

unsafe fn dispatch(hwnd: HWND, message: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if is_theme_notification(message) {
        // A hidden top-level window receives broadcasts; HWND_MESSAGE does not.
        // Never call TSF synchronously while the system broadcasts settings.
        let callback = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const Callback;
        if !callback.is_null() {
            let pending = unsafe { &(*callback).theme_pending };
            if !pending.replace(true)
                && !unsafe { PostMessageW(Some(hwnd), THEME_MESSAGE, WPARAM(0), LPARAM(0)) }
                    .as_bool()
            {
                pending.set(false);
            }
        }
        LRESULT(0)
    } else if message == EMOJI_MESSAGE {
        crate::keyboard::open_emoji_panel();
        LRESULT(0)
    } else if message == UPDATE_MESSAGE || message == THEME_MESSAGE {
        let callback = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const Callback;
        if !callback.is_null() {
            if message == THEME_MESSAGE {
                unsafe {
                    (*callback).theme_pending.set(false);
                }
            }
            // A TSF call can reenter Deactivate and destroy the window.
            let callback = unsafe { (*callback).invoke.clone() };
            callback();
        }
        LRESULT(0)
    } else {
        unsafe { DefWindowProcW(hwnd, message, wp, lp) }
    }
}

fn is_theme_notification(message: u32) -> bool {
    message == WM_SETTINGCHANGE as u32
        || message == WM_THEMECHANGED as u32
        || message == WM_SYSCOLORCHANGE as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn theme_broadcasts_are_deferred_without_interpreting_external_pointers() {
        assert!(is_theme_notification(WM_SETTINGCHANGE as u32));
        assert!(is_theme_notification(WM_THEMECHANGED as u32));
        assert!(is_theme_notification(WM_SYSCOLORCHANGE as u32));
        assert!(!is_theme_notification(UPDATE_MESSAGE));
    }
}
