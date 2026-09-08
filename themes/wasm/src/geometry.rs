//! Content-space geometry, independent of the native composition backend.
use crate::protocol::PanelStyle;

#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub(crate) struct Insets {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

pub(crate) fn insets(style: &PanelStyle) -> Insets {
    if style.shadow_radius <= 0.0 || style.color >> 24 == 0 {
        return Insets::default();
    }
    let spread = style.shadow_radius * 3.0;
    Insets {
        left: (spread - style.offset_x).max(0.0),
        top: (spread - style.offset_y).max(0.0),
        right: (spread + style.offset_x).max(0.0),
        bottom: (spread + style.offset_y).max(0.0),
    }
}

pub(crate) fn hit_content(x: f32, y: f32, width: f32, height: f32, radius: f32) -> bool {
    if x < 0.0 || y < 0.0 || x >= width || y >= height {
        return false;
    }
    let r = radius.min(width / 2.0).min(height / 2.0).max(0.0);
    let dx = x - x.clamp(r, width - r);
    let dy = y - y.clamp(r, height - r);
    dx * dx + dy * dy <= r * r
}
