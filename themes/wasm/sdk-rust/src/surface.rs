//! 展示表面的持久属性。create 可初始化；事件内修改须返回 Present，否则回滚。
//! 内容/定位/面板范围彼此独立，不包含绘图命令；详见 SDK 首页。
use crate::raw;
/// Persistent glass material. Sigma is DIP, weights sum to one, colors are ARGB.
/// The native host owns background sampling, effects and opaque fallback.
pub struct BackdropStyle {
    pub enabled: bool,
    pub tint: u32,
    pub blur_sigma: f32,
    pub backdrop_balance: f32,
    pub afterglow_balance: f32,
    pub color_balance: f32,
    pub fallback_color: u32,
}

pub fn set_backdrop(style: &BackdropStyle) {
    unsafe {
        raw::set_backdrop(
            style.enabled as i32,
            style.tint as i32,
            style.blur_sigma,
            style.backdrop_balance,
            style.afterglow_balance,
            style.color_balance,
            style.fallback_color as i32,
        );
    }
}

/// 全内容区域与定位区域分离。例如内容360×80，面板/anchor为(0,24,280,48)，
/// PNG装饰可绘制到(260,0,100,80)。panel只影响材质和阴影，不裁剪图片。
pub fn frame_geometry(width: f32, height: f32, anchor: [f32; 4]) {
    unsafe {
        raw::frame_geometry(width, height, anchor[0], anchor[1], anchor[2], anchor[3]);
    }
}
pub fn panel_bounds(rect: [f32; 4]) {
    unsafe {
        raw::panel_bounds(rect[0], rect[1], rect[2], rect[3]);
    }
}
pub fn set_size(w: f32, h: f32) {
    unsafe {
        raw::set_size(w, h);
    }
}
/// Show or hide a resident theme while retaining guest state.
pub fn set_visible(visible: bool) {
    unsafe { raw::set_visible(visible as i32) }
}
/// Place a resident theme at DIP offsets from the primary work area.
pub fn set_fixed_position(x: f32, y: f32) {
    unsafe { raw::set_fixed_position(x, y) }
}
/// Native panel: finite DIP corner radius 0..=4096, shadow radius 0..=250,
/// offsets -1024..=1024, ARGB color. These are host contract bounds.
/// Zero shadow radius disables shadow.
pub fn set_panel(radius: f32, shadow_radius: f32, offset_x: f32, offset_y: f32, color: u32) {
    unsafe { raw::set_panel(radius, shadow_radius, offset_x, offset_y, color as i32) }
}
