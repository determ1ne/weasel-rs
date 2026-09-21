//! 最短入门：配置 → 读取第一项 → 完整绘制 → 点击。
//! draw 的便捷函数自动缓存字体/布局；手动资源管理见 resources。
use weasel_wasm_sdk::{
    config::OPTIONS,
    draw::{draw, fill_rect, set_font},
    interaction::{Action, PointerPhase, hit_region, pointer_region, send_action},
    lifecycle::{ABI_VERSION, ErrorCode, EventKind, FrameResult},
    surface::set_size,
    view::View,
};

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
    let family = OPTIONS
        .string("/font")
        .unwrap_or_else(|| "Microsoft YaHei UI".into());
    set_font(0, &family);
    ErrorCode::Success as i32 // 此时仅初始化，不能绘图
}
fn paint() -> i32 {
    let item = View::read().and_then(|v| v.items.into_iter().next());
    if let Some(item) = item {
        set_size(240.0, 48.0);
        fill_rect(0.0, 0.0, 240.0, 48.0, 0xff202020);
        draw(&item.primary, 8.0, 8.0, 0, 20.0, 0xffffffff);
        if item.enabled {
            hit_region(1, 0.0, 0.0, 240.0, 48.0, 0.0);
        }
    }
    FrameResult::Present as i32 // 无候选时空帧清屏
}
#[unsafe(no_mangle)]
pub extern "C" fn theme_event(kind: i32, detail: i32, _x: f32, _y: f32, _now: f64) -> i32 {
    match EventKind::try_from(kind) {
        Ok(EventKind::View | EventKind::Appearance) => return paint(),
        Ok(EventKind::Pointer) => {
            // 最小示例在按下时选中；完整按钮应跟踪 Down/Up/Cancel。
            if detail == PointerPhase::Down as i32 && pointer_region() == 1 {
                send_action(Action::Item as i32, 0);
            }
        }
        Ok(EventKind::Hide | EventKind::Animation) => {}
        Err(_) => return ErrorCode::InvalidArgument as i32,
    }
    FrameResult::Keep as i32
}
