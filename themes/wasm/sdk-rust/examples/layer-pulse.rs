//! One finite native opacity transition per View event; no WASM frame loop.
use std::cell::Cell;
use weasel_wasm_sdk::{
    draw::{draw, fill_rect, set_font},
    layers::{Easing, LayerProperty, layer_animate, layer_remove, layer_set, with_layer},
    lifecycle::{ABI_VERSION, ErrorCode, EventKind, FrameResult},
    surface::set_size,
    view::View,
};
thread_local! { static HAS_LAYER: Cell<bool> = const { Cell::new(false) }; }
thread_local! { static BRIGHT_TARGET: Cell<bool> = const { Cell::new(false) }; }
fn remove_decoration() {
    HAS_LAYER.with(|has| {
        if has.replace(false) {
            layer_remove(1);
        }
    });
}
#[unsafe(no_mangle)]
pub extern "C" fn theme_abi_version() -> u32 {
    ABI_VERSION as u32
}
#[unsafe(no_mangle)]
pub extern "C" fn theme_capabilities() -> u32 {
    0
}
#[unsafe(no_mangle)]
pub extern "C" fn theme_create(_mode: i32, _dark: i32) -> i32 {
    set_font(0, "Microsoft YaHei UI");
    ErrorCode::Success as i32
}
#[unsafe(no_mangle)]
pub extern "C" fn theme_event(kind: i32, _detail: i32, _x: f32, _y: f32, _now: f64) -> i32 {
    let kind = match EventKind::try_from(kind) {
        Ok(kind) => kind,
        Err(_) => return ErrorCode::InvalidArgument as i32,
    };
    if kind == EventKind::Hide {
        HAS_LAYER.with(|has| has.set(false)); // Host clears layers on Hide.
        return FrameResult::Keep as i32;
    }
    if !matches!(kind, EventKind::View | EventKind::Appearance) {
        return FrameResult::Keep as i32;
    }
    let Some(view) = View::read().filter(|v| v.visible && !v.items.is_empty()) else {
        remove_decoration();
        fill_rect(0.0, 0.0, 1.0, 1.0, 0); // Replace main explicitly despite layer edits.
        return FrameResult::Present as i32;
    };
    set_size(240.0, 48.0);
    fill_rect(0.0, 0.0, 240.0, 48.0, 0xff202020);
    draw(&view.items[0].primary, 12.0, 8.0, 0, 20.0, 0xffffffff);
    HAS_LAYER.with(|has| {
        if !has.get() {
            with_layer(1, 240.0, 48.0, || {
                fill_rect(4.0, 8.0, 3.0, 32.0, 0xff60c0ff)
            });
            layer_set(1, LayerProperty::Opacity, 0.25); // 只在创建时设置初值。
            BRIGHT_TARGET.with(|bright| bright.set(false));
            has.set(true);
        }
    });
    if kind == EventKind::View {
        let target = BRIGHT_TARGET.with(|bright| {
            bright.set(!bright.get());
            if bright.get() { 1.0 } else { 0.25 }
        });
        // 后续 View 只改变目标，从当前显示值连续转向，不重置起点。
        layer_animate(1, LayerProperty::Opacity, target, 450.0, Easing::SmoothStep);
    }
    // Main replacement retains layer content and running animations.
    FrameResult::Present as i32
}
