//! 保留系统窗口样式和原生标题栏按钮，只将客户区延伸到顶部。
//! 子类过程不保存 Rust 对象指针，销毁时移除；缩放与系统菜单仍由 Windows 处理。
use crate::bindings::*;

const SUBCLASS_ID: usize = 0x575253;

pub fn install(hwnd: HWND) -> Result<(), String> {
    unsafe {
        if !SetWindowSubclass(hwnd, Some(window_proc), SUBCLASS_ID, 0).as_bool() {
            return Err("SetWindowSubclass failed".into());
        }
        let result = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            (SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE) as u32,
        );
        if !result.as_bool() {
            let _ = RemoveWindowSubclass(hwnd, Some(window_proc), SUBCLASS_ID);
            return Err("SetWindowPos(SWP_FRAMECHANGED) failed".into());
        }
    }
    Ok(())
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
    id: usize,
    _data: usize,
) -> LRESULT {
    unsafe {
        if msg == WM_NCDESTROY as u32 {
            let _ = RemoveWindowSubclass(hwnd, Some(window_proc), id);
            return DefSubclassProc(hwnd, msg, wp, lp);
        }
        if msg == WM_NCCALCSIZE as u32 && wp.0 != 0 {
            let params = lp.0 as *mut NCCALCSIZE_PARAMS;
            let top = (*params).rgrc[0].top;
            // 直接让系统计算原生按钮布局，避免 Winit 的客户区处理覆盖它。
            let _ = DefWindowProcW(hwnd, msg, wp, lp);
            // 最大化也不额外偏移客户区顶部，保持原生按钮布局坐标一致。
            // 否则手动命中和 DWM 内部悬停计算可能使用不同的按钮位置。
            (*params).rgrc[0].top = top;
            return LRESULT(0);
        }
        // 命中回退只解决点击；原生按钮悬停还需要完整的非客户区移动/离开链。
        // 在 DWM 可能提前返回前注册跟踪，离开时让 DWM 和系统同时清除高亮。
        if msg == WM_NCMOUSEMOVE as u32 {
            let mut tracking = TRACKMOUSEEVENT {
                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: (TME_NONCLIENT | TME_LEAVE) as u32,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            let _ = TrackMouseEvent(&mut tracking);
        }
        if msg == WM_NCMOUSELEAVE as u32 {
            let mut result = LRESULT(0);
            let _ = DwmDefWindowProc(hwnd, msg, wp, lp, &mut result);
            return DefSubclassProc(hwnd, msg, wp, lp);
        }
        let mut result = LRESULT(0);
        if DwmDefWindowProc(hwnd, msg, wp, lp, &mut result).as_bool() {
            return result;
        }
        if msg == WM_NCHITTEST as u32 {
            // DWM 在自定义客户区、最大化状态下可能不返回按钮命中结果。
            // 必须先检查可见按钮，不能先让标题拖动或 Winit 的边框命中覆盖它。
            if let Some(button) = caption_button(hwnd, lp) {
                return LRESULT(button as isize);
            }
            let hit = DefSubclassProc(hwnd, msg, wp, lp);
            if hit.0 != HTCLIENT as isize {
                return hit;
            }
            let mut rect = RECT::default();
            if !GetWindowRect(hwnd, &mut rect).as_bool() {
                return hit;
            }
            let dpi = GetDpiForWindow(hwnd).max(96);
            let x = (lp.0 as u16 as i16) as i32 - rect.left;
            let y = ((lp.0 >> 16) as u16 as i16) as i32 - rect.top;
            let border = GetSystemMetricsForDpi(SM_CYSIZEFRAME, dpi)
                + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi);
            if !IsZoomed(hwnd).as_bool() && y < border {
                return LRESULT(if x < border {
                    HTTOPLEFT
                } else if x >= rect.right - rect.left - border {
                    HTTOPRIGHT
                } else {
                    HTTOP
                } as isize);
            }
            // 与 Slint 顶部 8 + 48 DIP 区域一致；原生按钮优先交给 DWM。
            if y < (56 * dpi / 96) as i32 {
                return LRESULT(HTCAPTION as isize);
            }
            return hit;
        }
        DefSubclassProc(hwnd, msg, wp, lp)
    }
}

/// 最大化的窗口矩形包含屏幕外的缩放边框，按钮却位于显示器工作区内。
/// 使用 DWM 实际分配的按钮区域尺寸，并以可见工作区右上角为最大化时的基准。
unsafe fn caption_button(hwnd: HWND, lp: LPARAM) -> Option<i32> {
    unsafe {
        let mut bounds = RECT::default();
        if DwmGetWindowAttribute(
            hwnd,
            DWMWA_CAPTION_BUTTON_BOUNDS as u32,
            &mut bounds as *mut _ as _,
            size_of_val(&bounds) as u32,
        )
        .is_err()
            || bounds.right <= bounds.left
            || bounds.bottom <= bounds.top
        {
            return None;
        }
        let mut rect = RECT::default();
        if !GetWindowRect(hwnd, &mut rect).as_bool() {
            return None;
        }
        let maximized = IsZoomed(hwnd).as_bool();
        let (right, top) = if maximized {
            let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST as u32);
            let mut info = MONITORINFO {
                cbSize: size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if !GetMonitorInfoW(monitor, &mut info).as_bool() {
                return None;
            }
            (info.rcWork.right, info.rcWork.top)
        } else {
            (rect.left + bounds.right, rect.top + bounds.top)
        };
        let x = (lp.0 as u16 as i16) as i32;
        let y = ((lp.0 >> 16) as u16 as i16) as i32;
        let width = bounds.right - bounds.left;
        if x < right - width || x >= right || y < top || y >= top + bounds.bottom - bounds.top {
            return None;
        }
        // 原生普通窗口在保留最小化时也会保留最大化按钮槽（可能被禁用）。
        let style = GetWindowLongW(hwnd, GWL_STYLE);
        let slots = if style & (WS_MINIMIZEBOX | WS_MAXIMIZEBOX) != 0 {
            3
        } else {
            1
        };
        let slot = ((right - 1 - x) * slots / width).min(slots - 1);
        Some(match slot {
            0 => HTCLOSE,
            1 if style & WS_MAXIMIZEBOX != 0 => HTMAXBUTTON,
            2 if style & WS_MINIMIZEBOX != 0 => HTMINBUTTON,
            _ => HTCLIENT,
        })
    }
}
