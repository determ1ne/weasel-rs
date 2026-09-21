#![doc = include_str!("guide.md")]
pub mod types;
pub use types::{
    ABI_VERSION, Action, Capability, ConfigScope, DataKind as Kind, ErrorCode, EventKind,
    FrameResult, Mode, PointerPhase,
};
pub mod animation;
pub mod config;
pub mod diagnostics;
pub mod draw;
mod graphics;
pub mod interaction;
pub mod layers;
pub mod lifecycle;
/// 原始宿主 ABI；常规主题优先使用分组 SDK。
pub mod raw;
pub mod resources;
pub mod surface;
pub mod view;

// 便捷根导出暂保留；实现已按职责移到各模块，新主题建议使用分组导入。
pub use config::{Data, OPTIONS, SETTINGS};
pub use diagnostics::{log, report_notice};
pub use draw::*;
pub use interaction::{begin_drag, hit_region, pointer_region, send_action};
pub use resources::{Font, Image, TextLayout};
pub use surface::*;
pub use view::View;
pub const FONT_TEXT_BOLD: i32 = 4;
pub const ACTION_DISMISS: i32 = Action::Dismiss as i32;

// 保留旧的根导出；新主题使用 animation 模块。
pub fn request_frame() {
    unsafe {
        raw::request_frame();
    }
}
pub fn time_ms() -> f64 {
    unsafe { raw::time_ms() }
}
