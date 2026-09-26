//! 导出不创建窗口的 `void` 原生主题。
//!
//! 本主题不显示候选 UI，也不产生交互事件；输入和键盘选词继续由宿主既有链路处理。
pub use weasel_theme_api as theme_api;
#[path = "mod.rs"]
mod theme;
weasel_theme_api::export_theme!(theme::Factory);
