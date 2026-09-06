//! Shared snapshot visibility and screen-space placement, independent of toolkit.
use crate::bindings::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromRect, RECT,
};
use weasel_common::message::{RenderRect, RenderSnapshot};

pub fn is_visible(snapshot: &RenderSnapshot) -> bool {
    snapshot.visible
        && !snapshot.items.is_empty()
        && snapshot.anchor.as_ref().is_some_and(|anchor| anchor.valid)
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
        let mut snapshot = RenderSnapshot::default();
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
