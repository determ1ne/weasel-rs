//! ten 主题 DLL 入口；原生窗口和图形资源始终由创建它们的 UI 线程管理。
//!
//! 本 crate 复用公共主题 API 与 Windows/Direct2D 支持层，并导出内部主题工厂。
pub use weasel_theme_api as theme_api;
pub use weasel_theme_support::{appearance, bindings, d2d_bindings, diagnostics, presentation};
#[path = "mod.rs"]
mod theme;
weasel_theme_api::export_theme!(theme::Factory);
