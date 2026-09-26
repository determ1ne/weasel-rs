//! 在 XAML Island 上方创建原生输入层，参考 Windows Terminal 的非客户区 Island 窗口实现。
//!
//! 输入层将空白区域转发为标题栏拖动，同时通过原生区域排除交互控件；不合成 `SC_MOVE`，
//! 也不依赖 XAML 指针捕获。
#![allow(unsafe_op_in_unsafe_fn)]
use crate::ui_bindings::Windows::{
    Foundation::Rect,
    UI::Xaml::{
        Controls::{Button, Grid, ScrollViewer},
        FrameworkElement, Visibility,
    },
    Win32::*,
};
use std::cell::{Cell, RefCell};
use windows_core::Interface;
use windows_strings::{PCWSTR, w};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// 以父窗口客户区像素表示的矩形边界，右侧和下侧作为排他边界交给 GDI 区域运算。
struct Bounds {
    /// 左边界。
    left: i32,
    /// 上边界。
    top: i32,
    /// 右侧排他边界。
    right: i32,
    /// 下侧排他边界。
    bottom: i32,
}

/// 覆盖部署窗口空白区域的原生输入子窗口，并从命中区域中扣除可交互控件。
///
/// 所有 UI 与 HWND 操作都应在创建该层的窗口线程执行。更新期间以 `updating` 防止布局
/// 回调重入；缓存仅在区域、窗口尺寸均匹配时复用。发生布局错误时隐藏输入层并清空缓存，
/// 避免过期区域截获控件输入。
pub struct DragLayer {
    /// 接收非客户区鼠标消息的透明子窗口。
    window: HWND,
    /// 拥有输入层的部署顶层窗口。
    parent: HWND,
    /// 用于将控件坐标变换到共同的 XAML 坐标系。
    root: Grid,
    /// 日志滚动区域；其矩形从拖动命中区域中排除。
    scroll: FrameworkElement,
    /// 部署完成后显示的确认按钮；仅可见且布局完成时排除。
    button: FrameworkElement,
    /// 最近成功应用的客户区尺寸及控件排除矩形。
    cached: RefCell<Option<(i32, i32, Vec<Bounds>)>>,
    /// 布局更新重入保护，仅供窗口线程访问。
    updating: Cell<bool>,
}

impl DragLayer {
    /// 创建输入子窗口并设置其非视觉 layered window 属性。
    ///
    /// `parent`、XAML 元素及本对象必须具有一致的 UI 线程生命周期。初始化失败返回错误；
    /// HWND 一旦创建便立即由临时 `DragLayer` 接管，后续配置失败也会销毁它。
    pub unsafe fn new(
        parent: HWND,
        root: &Grid,
        scroll: &ScrollViewer,
        button: &Button,
    ) -> Result<Self, String> {
        let scroll = scroll.cast().map_err(|e| e.to_string())?;
        let button = button.cast().map_err(|e| e.to_string())?;
        let name = w!("WeaselRS.DeployDragInput");
        let class = WNDCLASSW {
            lpfnWndProc: Some(input_proc),
            hInstance: GetModuleHandleW(None),
            lpszClassName: name,
            hCursor: LoadCursorW(None, IDC_ARROW),
            ..Default::default()
        };
        RegisterClassW(&class);
        let window = CreateWindowExW(
            (WS_EX_LAYERED | WS_EX_NOREDIRECTIONBITMAP) as u32,
            name,
            PCWSTR::null(),
            WS_CHILD as u32,
            0,
            0,
            0,
            0,
            Some(parent),
            None,
            Some(class.hInstance),
            None,
        );
        if window.0.is_null() {
            return Err(std::io::Error::last_os_error().to_string());
        }
        // Own immediately, including on subsequent initialization failure.
        let layer = Self {
            window,
            parent,
            root: root.clone(),
            scroll,
            button,
            cached: RefCell::new(None),
            updating: Cell::new(false),
        };
        SetWindowLongPtrW(window, GWLP_USERDATA, parent.0 as isize);
        // Same material-neutral input window flags/alpha as Terminal. Do not
        // use alpha=0 or WS_EX_TRANSPARENT: those pass mouse input through.
        SetLayeredWindowAttributes(window, COLORREF(0), 255, LWA_ALPHA as u32)
            .ok()
            .map_err(|e| e.to_string())?;
        Ok(layer)
    }

    /// 返回该输入层的 HWND，供消息循环决定是否先将消息交给 XAML。
    pub fn hwnd(&self) -> HWND {
        self.window
    }

    /// 根据当前客户区和可见控件几何更新原生命中区域。
    ///
    /// XAML 首次布局尚未完成时暂时隐藏输入层并等待后续布局通知。任一 Win32/XAML/GDI
    /// 操作失败都会隐藏输入层、清除缓存并返回错误；重入调用直接返回，不递归更新。
    pub unsafe fn update(&self) -> Result<(), String> {
        if self.updating.replace(true) {
            return Ok(());
        }
        let result = self.update_inner();
        if result.is_err() {
            // A stale exclusion must never steal input from a control.
            let _ = ShowWindow(self.window, SW_HIDE);
            *self.cached.borrow_mut() = None;
        }
        self.updating.set(false);
        result
    }

    /// 计算并应用拖动区域；控件矩形来自 XAML 逻辑坐标，最终转换为客户区像素。
    ///
    /// 可见控件尺寸为零表示布局尚未就绪，此时隐藏窗口并等待下一次更新。相同尺寸和排除
    /// 矩形会命中缓存以避免重复创建 GDI 区域；设置成功后区域句柄所有权转交给 USER。
    unsafe fn update_inner(&self) -> Result<(), String> {
        let mut client = RECT::default();
        GetClientRect(self.parent, &mut client)
            .ok()
            .map_err(|e| e.to_string())?;
        let scale = GetDpiForWindow(self.parent).max(96) as f64 / 96.0;
        if self.scroll.ActualWidth().map_err(|e| e.to_string())? <= 0.0
            || self.scroll.ActualHeight().map_err(|e| e.to_string())? <= 0.0
        {
            let _ = ShowWindow(self.window, SW_HIDE);
            *self.cached.borrow_mut() = None;
            return Ok(()); // Wait for XAML's first completed layout.
        }
        let mut holes = Vec::new();
        for element in [&self.scroll, &self.button] {
            if element.Visibility().map_err(|e| e.to_string())? != Visibility::Visible {
                continue;
            }
            let width = element.ActualWidth().map_err(|e| e.to_string())?;
            let height = element.ActualHeight().map_err(|e| e.to_string())?;
            if width <= 0.0 || height <= 0.0 {
                let _ = ShowWindow(self.window, SW_HIDE);
                *self.cached.borrow_mut() = None;
                return Ok(()); // Newly visible button has not been laid out yet.
            }
            let rect = element
                .TransformToVisual(&self.root)
                .and_then(|t| {
                    t.TransformBounds(Rect {
                        X: 0.0,
                        Y: 0.0,
                        Width: width as f32,
                        Height: height as f32,
                    })
                })
                .map_err(|e| e.to_string())?;
            holes.push(pixel_bounds(rect, scale));
        }
        let key = (client.right, client.bottom, holes);
        if self.cached.borrow().as_ref() == Some(&key) {
            return Ok(());
        }
        if key.0 <= 0 || key.1 <= 0 {
            let _ = ShowWindow(self.window, SW_HIDE);
            *self.cached.borrow_mut() = Some(key);
            return Ok(());
        }
        let region = input_region(key.0, key.1, &key.2)?;
        if SetWindowRgn(self.window, Some(region.0), true.into()) == 0 {
            return Err("SetWindowRgn failed for drag input layer".into());
        }
        // USER owns the region after success; failed calls retain our ownership.
        std::mem::forget(region);
        SetWindowPos(
            self.window,
            Some(HWND_TOP),
            0,
            0,
            key.0,
            key.1,
            (SWP_NOACTIVATE | SWP_SHOWWINDOW) as u32,
        )
        .ok()
        .map_err(|e| e.to_string())?;
        *self.cached.borrow_mut() = Some(key);
        Ok(())
    }
}
impl Drop for DragLayer {
    /// 销毁输入层前清空窗口过程使用的父窗口指针。
    fn drop(&mut self) {
        unsafe {
            SetWindowLongPtrW(self.window, GWLP_USERDATA, 0);
            let _ = DestroyWindow(self.window);
        }
    }
}
/// 用一个矩形区域表示 GDI 区域句柄，并在失败或离开作用域时释放句柄。
struct Region(HRGN);
/// 创建覆盖整个客户区的拖动区域，再逐个减去控件矩形。
///
/// 返回的句柄由 `Region` 独占；只有调用方成功交给 `SetWindowRgn` 后才会转移所有权。
unsafe fn input_region(width: i32, height: i32, holes: &[Bounds]) -> Result<Region, String> {
    let region = Region::new(Bounds {
        left: 0,
        top: 0,
        right: width,
        bottom: height,
    })?;
    for hole in holes {
        let excluded = Region::new(*hole)?;
        if CombineRgn(Some(region.0), Some(region.0), Some(excluded.0), RGN_DIFF) == 0 {
            return Err("CombineRgn failed for drag exclusion".into());
        }
    }
    Ok(region)
}
impl Region {
    /// 创建一个 GDI 矩形区域；系统分配失败时返回错误。
    unsafe fn new(rect: Bounds) -> Result<Self, String> {
        let handle = CreateRectRgn(rect.left, rect.top, rect.right, rect.bottom);
        if handle.0.is_null() {
            Err("CreateRectRgn failed".into())
        } else {
            Ok(Self(handle))
        }
    }
}
impl Drop for Region {
    /// 释放仍由本对象持有的 GDI 区域句柄。
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(HGDIOBJ(self.0.0));
        }
    }
}
/// 将 XAML 逻辑矩形按 DPI 缩放成像素矩形，并向外取整以免边缘覆盖交互控件。
fn pixel_bounds(rect: Rect, scale: f64) -> Bounds {
    // Round outward so a fractional-DPI control edge is never intercepted.
    Bounds {
        left: (rect.X as f64 * scale).floor() as i32,
        top: (rect.Y as f64 * scale).floor() as i32,
        right: ((rect.X as f64 + rect.Width as f64) * scale).ceil() as i32,
        bottom: ((rect.Y as f64 + rect.Height as f64) * scale).ceil() as i32,
    }
}
/// 将输入层的客户区命中和非客户区鼠标消息转交父窗口，使空白区域可拖动顶层窗口。
///
/// 对客户区命中改报标题栏命中；缩放边缘等其他命中结果保持父窗口的判定。
unsafe extern "system" fn input_proc(hwnd: HWND, message: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let parent = HWND(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut _);
    if !parent.0.is_null() {
        if message == WM_NCHITTEST as u32 {
            let hit = SendMessageW(parent, message, wp, lp);
            // Every part of this region is draggable except resize edges;
            // interactive XAML controls are physically absent from the region.
            return if hit.0 == HTCLIENT as isize {
                LRESULT(HTCAPTION as isize)
            } else {
                hit
            };
        }
        if message == WM_NCLBUTTONDOWN as u32
            || message == WM_NCLBUTTONUP as u32
            || message == WM_NCLBUTTONDBLCLK as u32
            || message == WM_NCMOUSEMOVE as u32
        {
            return SendMessageW(parent, message, wp, lp);
        }
    }
    DefWindowProcW(hwnd, message, wp, lp)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_region_excludes_controls_and_restores_hidden_button_area() {
        // GDI region operations only: no HWND, XAML initialization or GUI.
        let log = Bounds {
            left: 36,
            top: 110,
            right: 684,
            bottom: 390,
        };
        let button = Bounds {
            left: 600,
            top: 420,
            right: 696,
            bottom: 460,
        };
        unsafe {
            let completed = input_region(720, 480, &[log, button]).unwrap();
            for (x, y) in [(100, 20), (10, 200), (400, 440), (1, 1)] {
                assert!(PtInRegion(completed.0, x, y).as_bool());
            }
            assert!(!PtInRegion(completed.0, 100, 200).as_bool());
            assert!(!PtInRegion(completed.0, 650, 440).as_bool());
            assert!(!PtInRegion(completed.0, 720, 480).as_bool());
            let deploying = input_region(720, 480, &[log]).unwrap();
            assert!(PtInRegion(deploying.0, 650, 440).as_bool());
        }
    }
    #[test]
    fn fractional_dpi_exclusions_round_outward() {
        assert_eq!(
            pixel_bounds(
                Rect {
                    X: 10.25,
                    Y: 20.5,
                    Width: 96.0,
                    Height: 40.0
                },
                1.5
            ),
            Bounds {
                left: 15,
                top: 30,
                right: 160,
                bottom: 91
            }
        );
    }
}
