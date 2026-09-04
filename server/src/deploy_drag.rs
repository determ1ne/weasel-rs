//! Native input sink above the Island, following Windows Terminal's
//! NonClientIslandWindow.cpp. No synthetic SC_MOVE or XAML pointer capture.
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
struct Bounds {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

pub struct DragLayer {
    window: HWND,
    parent: HWND,
    root: Grid,
    scroll: FrameworkElement,
    button: FrameworkElement,
    cached: RefCell<Option<(i32, i32, Vec<Bounds>)>>,
    updating: Cell<bool>,
}

impl DragLayer {
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

    pub fn hwnd(&self) -> HWND {
        self.window
    }

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
    fn drop(&mut self) {
        unsafe {
            SetWindowLongPtrW(self.window, GWLP_USERDATA, 0);
            let _ = DestroyWindow(self.window);
        }
    }
}
struct Region(HRGN);
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
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(HGDIOBJ(self.0.0));
        }
    }
}
fn pixel_bounds(rect: Rect, scale: f64) -> Bounds {
    // Round outward so a fractional-DPI control edge is never intercepted.
    Bounds {
        left: (rect.X as f64 * scale).floor() as i32,
        top: (rect.Y as f64 * scale).floor() as i32,
        right: ((rect.X as f64 + rect.Width as f64) * scale).ceil() as i32,
        bottom: ((rect.Y as f64 + rect.Height as f64) * scale).ceil() as i32,
    }
}
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
