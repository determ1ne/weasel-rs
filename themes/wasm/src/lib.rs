//! 面向 Windows renderer 的 WASM 主题 DLL。
//!
//! 主题模块以 WASM 形式提供，由 Wasmtime 执行并驱动候选窗界面。主要职责划分：
//!
//! - [`protocol`] 定义 WASM 与宿主之间的导入、导出和绘制协议。
//! - [`runtime`] 管理 Wasmtime 实例、宿主状态、资源及陷阱收敛。
//! - [`canvas`] 封装 Direct2D/DirectWrite 绘制能力。
//! - [`window`] 管理 HWND、DPI、消息分发、计时器和设备丢失恢复。
//! - [`layers`] 与 [`layer_api`] 实现事务化保留图层模型及其宿主导入。
//! - [`animation`] 汇总事件请求并安排窗口唤醒。
//! - [`backend`] 将窗口接入通用 `ThemeBackend` 接口。
//!
//! 主题加载或初始化失败时，工厂返回错误供 renderer 走既有回退路径。
pub use weasel_theme_api as theme_api;
pub use weasel_theme_support::{appearance, bindings, d2d_bindings, presentation};

#[path = "../sdk-rust/src/types.rs"]
mod abi;
mod animation;
pub mod backend;
pub mod canvas;
mod composition;
mod data;
mod geometry;
mod glass;
mod layer_api;
mod layers;
pub mod protocol;
mod resources;
pub mod runtime;
pub mod window;

#[path = "mod.rs"]
mod theme;

weasel_theme_api::export_theme!(theme::Factory);
