//! Shared snapshot visibility and screen-space placement, independent of toolkit.
use crate::bindings::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromRect, RECT,
};
use crate::theme_api::{Anchor as RenderRect, CandidateView};

pub fn is_visible(snapshot: &CandidateView) -> bool {
    snapshot.visible
        && !snapshot.items.is_empty()
        && snapshot.anchor.as_ref().is_some_and(|anchor| anchor.valid)
}

/// Center a `width`x`height` (device pixels) window on the primary monitor's
/// work area. Used only by the standalone preview, which has no caret anchor.
pub fn preview_position(width: i32, height: i32) -> (i32, i32) {
    unsafe {
        // A 1x1 rect at the virtual-screen origin resolves to the monitor that
        // hosts (0,0), which is the primary display in the common layout.
        let origin = RECT {
            left: 0,
            top: 0,
            right: 1,
            bottom: 1,
        };
        let monitor = MonitorFromRect(&origin, MONITOR_DEFAULTTONEAREST as u32);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if monitor.0.is_null() || !GetMonitorInfoW(monitor, &mut info).as_bool() {
            return (0, 0);
        }
        let work = &info.rcWork;
        let x = work.left + (work.right - work.left - width) / 2;
        let y = work.top + (work.bottom - work.top - height) / 2;
        (x.max(work.left), y.max(work.top))
    }
}

pub fn popup_position(anchor: &RenderRect, width: i32, height: i32) -> (i32, i32) {
    unsafe {
        let rect = RECT {
            left: anchor.left,
            top: anchor.top,
            right: anchor.right,
            bottom: anchor.bottom,
        };
        let monitor = MonitorFromRect(&rect, MONITOR_DEFAULTTONEAREST as u32);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if monitor.0.is_null() || !GetMonitorInfoW(monitor, &mut info).as_bool() {
            return (anchor.left, anchor.bottom);
        }
        within_work_area(anchor, width, height, &info.rcWork)
    }
}

fn within_work_area(anchor: &RenderRect, width: i32, height: i32, work: &RECT) -> (i32, i32) {
    let y = if anchor.bottom.saturating_add(height) > work.bottom {
        anchor.top.saturating_sub(height)
    } else {
        anchor.bottom
    };
    let x = if anchor.left.saturating_add(width) > work.right {
        work.right.saturating_sub(width)
    } else {
        anchor.left
    };
    (x.max(work.left), y.max(work.top))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clamps_to_work_area_and_handles_negative_monitors() {
        let anchor = RenderRect {
            left: -20,
            right: -10,
            top: 980,
            bottom: 1000,
            valid: true,
        };
        let work = RECT {
            left: -1920,
            right: 0,
            top: 0,
            bottom: 1040,
        };
        assert_eq!(within_work_area(&anchor, 300, 69, &work), (-300, 911));
        assert_eq!(within_work_area(&anchor, 3000, 1200, &work), (-1920, 0));
    }
    #[test]
    fn visibility_requires_items_and_an_anchor() {
        let mut snapshot = CandidateView::default();
        snapshot.visible = true;
        snapshot.items.push(Default::default());
        assert!(!is_visible(&snapshot));
        snapshot.anchor = Some(RenderRect {
            valid: true,
            ..Default::default()
        });
        assert!(is_visible(&snapshot));
        snapshot.visible = false;
        assert!(!is_visible(&snapshot));
    }
}
