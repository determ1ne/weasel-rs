//! 在 TSF 所属线程上分派 RPC 状态通知与延后操作。
//!
//! 隐藏顶层窗口负责接收系统主题广播，并把需访问 TSF 的工作排入窗口消息队列；
//! 管道工作线程不直接调用宿主或 TSF。窗口及其回调状态必须在同一线程创建和销毁。
use crate::bindings::*;
use std::{cell::Cell, rc::Rc};
use windows_core::{Error, Result};
use windows_strings::{HSTRING, PCWSTR};

/// TSF 线程投递 RPC 状态更新的窗口消息编号。
pub const UPDATE_MESSAGE: u32 = WM_APP as u32 + 23;
/// 维护定时器在 `SetTimer`/`KillTimer` 中使用的定时器标识。
pub const MAINTENANCE_TIMER: usize = 1;
const EMOJI_MESSAGE: u32 = UPDATE_MESSAGE + 1;
const THEME_MESSAGE: u32 = UPDATE_MESSAGE + 2;

struct Callback {
    /// 克隆到局部变量后再调用，避免回调重入销毁窗口时继续借用其状态。
    invoke: Rc<dyn Fn()>,
    /// 合并尚未分派的主题变更通知，避免广播期间同步重入 TSF。
    theme_pending: Cell<bool>,
}

/// 在 TSF 线程上承载异步通知的隐藏 Win32 窗口。
///
/// `callback` 的堆地址在窗口存活期间写入 `GWLP_USERDATA`；析构先清除该指针并销毁
/// 窗口，再释放回调，因而窗口过程不会访问已释放的 Rust 状态。
pub struct UpdateWindow {
    /// 隐藏顶层窗口句柄；仅由创建它的 TSF 线程使用和销毁。
    pub hwnd: HWND,
    /// 在窗口存活期内保持地址稳定，供 `GWLP_USERDATA` 指回窗口回调状态。
    callback: Box<Callback>,
    /// 注册窗口类时使用的名称，析构时用于注销同一类。
    class_name: HSTRING,
}

impl UpdateWindow {
    /// 将系统表情面板快捷键排入窗口队列。
    ///
    /// 调用方应在 `EndComposition` 成功后调用，使编辑会话释放写锁后才向 Shell 注入输入。
    /// 投递失败只写入标准错误，不会跨越窗口过程或 COM 边界传播错误。
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

    /// 注册并创建接收通知的隐藏顶层窗口。
    ///
    /// 回调须为 `'static`，且由创建窗口的 TSF 线程执行。注册类或创建窗口失败时返回
    /// Win32 错误，并撤销已完成的类注册；成功后由 `Drop` 配对销毁窗口和类。
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
    /// 清除窗口用户数据后销毁 HWND，并注销本实例的窗口类。
    fn drop(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), MAINTENANCE_TIMER);
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

/// Win32 窗口过程 ABI 入口；捕获 Rust panic，绝不向系统回调栈传播异常。
unsafe extern "system" fn window_proc(hwnd: HWND, message: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    crate::boundary::guard(None, || Ok(unsafe { dispatch(hwnd, message, wp, lp) }))
        .unwrap_or(LRESULT(0))
}

/// 处理窗口消息：主题广播只排队，维护与用户操作则在本窗口线程分派。
///
/// TSF 回调可能重入停用并销毁窗口，因此先克隆回调句柄再调用；系统广播期间不直接进入
/// TSF，未知消息交还给默认窗口过程。
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
    } else if message == UPDATE_MESSAGE
        || message == THEME_MESSAGE
        || (message == WM_TIMER as u32 && wp.0 == MAINTENANCE_TIMER)
    {
        if message == WM_TIMER as u32 {
            unsafe {
                let _ = KillTimer(Some(hwnd), MAINTENANCE_TIMER);
            }
        }
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

/// 识别需要延后处理的系统主题和配色广播消息。
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
