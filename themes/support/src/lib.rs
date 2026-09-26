//! 为原生主题提供共享的系统绑定、外观查询和屏幕几何工具。
//!
//! 本 crate 重导出主题 API，并集中维护多个原生主题共用的支持代码；它不负责主题注册、
//! RPC 路由或主题后端生命周期管理。
pub use weasel_theme_api as theme_api;
pub mod appearance;
pub mod bindings;
pub mod d2d_bindings;
pub mod presentation;
pub mod diagnostics {
    pub fn record(message: std::fmt::Arguments<'_>) {
        eprintln!("weasel-renderer: {message}");
    }
}
