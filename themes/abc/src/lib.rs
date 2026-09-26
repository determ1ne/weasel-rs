//! ABC 输入法主题 DLL 的入口。
//!
//! 本 crate 重导出主题 API 与 Windows 绘制、外观和定位支持，并将主题工厂交给导出宏；
//! Direct2D 等原生资源由主题后端在创建它们的 UI 线程上持有和释放。
pub use weasel_theme_api as theme_api;
pub use weasel_theme_support::{appearance, bindings, d2d_bindings, diagnostics, presentation};
#[path = "mod.rs"]
mod theme;
weasel_theme_api::export_theme!(theme::Factory);
