//! 在事件回调中构建当前主画面的绘制命令。
//!
//! 绘制命令在每次事件开始时重置；返回 `FrameResult::Present` 才会提交主画面，且普通
//! 主画面 Present 会替换整帧。变换和裁剪只影响绘制，不会改变表面几何或命中区域；
//! 每次 push 都必须配对 pop，即使本次事件最终返回 `Keep` 也须恢复栈平衡。
pub use crate::graphics::{draw, draw_glow, line_height, measure, set_font};
use crate::raw;
/// 推入局部到父坐标的仿射变换 `[m11, m12, m21, m22, dx, dy]`。
///
/// 变换可嵌套，影响后续绘制命令；最多 32 层，提交前必须全部弹出。
pub fn push_transform(matrix: [f32; 6]) {
    unsafe {
        raw::push_transform(
            matrix[0], matrix[1], matrix[2], matrix[3], matrix[4], matrix[5],
        );
    }
}
/// 推入当前坐标系中的矩形裁剪 `[x, y, width, height]`；旋转变换下按轴对齐包围框裁剪。
pub fn push_clip(rect: [f32; 4]) {
    unsafe {
        raw::push_clip(rect[0], rect[1], rect[2], rect[3]);
    }
}
/// 弹出最近推入的裁剪或变换；栈为空或事件结束时仍有未闭合状态会导致宿主 trap。
pub fn pop_draw_state() {
    unsafe {
        raw::pop_draw_state();
    }
}

/// 绘制实心矩形；坐标和尺寸以 DIP 表示，颜色为非预乘 `0xAARRGGBB`。
pub fn fill_rect(x: f32, y: f32, w: f32, h: f32, color: u32) {
    unsafe {
        raw::fill_rect(x, y, w, h, color);
    }
}

/// 绘制单个原生圆角矩形；圆角半径由宿主限制在矩形范围内。
pub fn rounded_rect(x: f32, y: f32, w: f32, h: f32, radius: f32, color: u32) {
    unsafe {
        raw::fill_rounded_rect(x, y, w, h, radius, color);
    }
}
/// 绘制矩形边框；几何量以 DIP 表示，颜色为非预乘 `0xAARRGGBB`。
pub fn stroke_rect(x: f32, y: f32, w: f32, h: f32, color: u32, width: f32) {
    unsafe {
        raw::stroke_rect(x, y, w, h, color, width);
    }
}
