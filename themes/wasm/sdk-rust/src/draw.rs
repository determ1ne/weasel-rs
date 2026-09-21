//! 每次事件的完整绘制命令。仅在 theme_event 使用，返回 Present 后替换旧画面。
//! 变换和裁剪仅作用于绘制，push 必须与 pop 配对。
pub use crate::graphics::{draw, draw_glow, line_height, measure, set_font};
use crate::raw;
/// 局部到父坐标的仿射变换，顺序为m11,m12,m21,m22,dx,dy。
pub fn push_transform(matrix: [f32; 6]) {
    unsafe {
        raw::push_transform(
            matrix[0], matrix[1], matrix[2], matrix[3], matrix[4], matrix[5],
        );
    }
}
pub fn push_clip(rect: [f32; 4]) {
    unsafe {
        raw::push_clip(rect[0], rect[1], rect[2], rect[3]);
    }
}
pub fn pop_draw_state() {
    unsafe {
        raw::pop_draw_state();
    }
}

pub fn fill_rect(x: f32, y: f32, w: f32, h: f32, color: u32) {
    unsafe {
        raw::fill_rect(x, y, w, h, color);
    }
}

/// Fill a single native rounded rectangle, with radius clamped to its bounds.
pub fn rounded_rect(x: f32, y: f32, w: f32, h: f32, radius: f32, color: u32) {
    unsafe {
        raw::fill_rounded_rect(x, y, w, h, radius, color);
    }
}
pub fn stroke_rect(x: f32, y: f32, w: f32, h: f32, color: u32, width: f32) {
    unsafe {
        raw::stroke_rect(x, y, w, h, color, width);
    }
}
