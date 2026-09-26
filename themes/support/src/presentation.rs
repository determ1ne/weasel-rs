//! 提供与具体绘制工具包无关的快照显隐判断和屏幕坐标定位。
//!
//! 锚点与窗口尺寸使用物理像素；固定位置的配置偏移则以 DIP 表示，并按调用方提供的
//! DPI 换算。定位优先使用锚点所在显示器的工作区，无法查询显示器信息时采用明确的回退值。
use crate::bindings::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromRect, RECT,
};
use crate::theme_api::{Anchor as RenderRect, CandidateView};

/// 判断完整快照是否包含可展示内容及有效定位锚点。
///
/// 快照必须要求可见，并且至少包含候选或预编辑文本；锚点缺失或无效时结果为 `false`。
pub fn is_visible(snapshot: &CandidateView) -> bool {
    snapshot.visible
        && (!snapshot.items.is_empty() || snapshot.preedit.is_some())
        && snapshot.anchor.as_ref().is_some_and(|anchor| anchor.valid)
}

/// 判断快照是否包含位于有效插入点附近的中英文模式提示。
///
/// 此函数只判断主题输入模型，不决定主题是否声明了对应能力；能力门控由 Renderer
/// 和具体主题工厂共同完成。
pub fn is_mode_indicator_visible(snapshot: &CandidateView) -> bool {
    snapshot.mode_indicator.is_some()
        && snapshot.active
        && snapshot.anchor.as_ref().is_some_and(|anchor| anchor.valid)
}

/// 将独立预览窗口居中放在主显示器工作区内；预览没有插入点锚点可供定位。
///
/// `width`、`height` 和返回坐标均为物理像素；无法取得显示器信息时返回屏幕原点。
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

/// 将驻留窗口放在主显示器工作区原点的 DIP 偏移处，并限制在工作区内。
///
/// `width`、`height` 和返回坐标均为物理像素；`x`、`y` 是 DIP 偏移。换算时 DPI 至少按
/// 1 处理。无法取得显示器信息时，返回未缩放、四舍五入后的偏移。
pub fn fixed_position(x: f32, y: f32, width: i32, height: i32, dpi: u32) -> (i32, i32) {
    unsafe {
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
            return (x.round() as i32, y.round() as i32);
        }
        let scale = dpi.max(1) as f32 / 96.0;
        let requested_x = info.rcWork.left.saturating_add((x * scale).round() as i32);
        let requested_y = info.rcWork.top.saturating_add((y * scale).round() as i32);
        clamp_fixed(requested_x, requested_y, width, height, &info.rcWork)
    }
}

/// 将固定窗口坐标夹限在工作区内；窗口大于工作区时对齐工作区左上边界。
fn clamp_fixed(x: i32, y: i32, width: i32, height: i32, work: &RECT) -> (i32, i32) {
    let max_x = work.right.saturating_sub(width).max(work.left);
    let max_y = work.bottom.saturating_sub(height).max(work.top);
    (x.clamp(work.left, max_x), y.clamp(work.top, max_y))
}

/// 根据锚点所在显示器的工作区放置弹出窗口，优先显示在锚点下方。
///
/// 尺寸及锚点边界均为物理像素。下方空间不足时尝试放在锚点上方，水平方向必要时
/// 向左收进工作区；显示器信息不可用时退回锚点左下方。
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

/// 返回锚点所在显示器的物理像素工作区，供多窗口主题共同约束窗口位置。
///
/// 显示器查询失败时返回 `None`；返回矩形标记为有效。
pub fn work_area(anchor: &RenderRect) -> Option<RenderRect> {
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
            return None;
        }
        Some(RenderRect {
            left: info.rcWork.left,
            top: info.rcWork.top,
            right: info.rcWork.right,
            bottom: info.rcWork.bottom,
            valid: true,
        })
    }
}

/// 在工作区内计算弹出窗口位置；边界运算采用饱和加减以避免整数溢出。
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

    #[test]
    fn preedit_can_be_visible_without_candidates() {
        let mut view = CandidateView {
            visible: true,
            preedit: Some(crate::theme_api::Preedit {
                text: "ni".into(),
                cursor: 2,
            }),
            anchor: Some(RenderRect {
                valid: true,
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(is_visible(&view));
        view.preedit = None;
        assert!(!is_visible(&view));
    }

    #[test]
    fn fixed_position_is_clamped_to_the_work_area() {
        let work = RECT {
            left: -1920,
            top: 0,
            right: 0,
            bottom: 1040,
        };
        assert_eq!(clamp_fixed(-1800, 40, 450, 72, &work), (-1800, 40));
        assert_eq!(clamp_fixed(500, -10, 450, 72, &work), (-450, 0));
    }
}
