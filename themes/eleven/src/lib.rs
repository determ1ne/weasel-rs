//! eleven Windows 11 候选窗主题 DLL 的 crate 入口。
//!
//! 通过主题 API 导出主题工厂，并复用主题支持库提供的绑定、外观、诊断和窗口
//! 定位能力。窗口与 XAML 原生资源由各自创建它们的 UI 线程持有和释放。
pub use weasel_theme_api as theme_api;
pub use weasel_theme_support::{appearance, bindings, d2d_bindings, diagnostics, presentation};
#[path = "mod.rs"]
mod theme;
weasel_theme_api::export_theme!(theme::Factory);
