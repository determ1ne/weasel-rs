//! WASM 主题 DLL。
//!
//! 用户以 AssemblyScript 编写主题、编译为 .wasm；本 DLL 用 wasmtime 加载并
//! 驱动候选窗 UI，各层职责：
//! - [`protocol`]：WASM↔Host ABI 契约（导入/导出名、常量、命令类型）
//! - [`runtime`]：wasmtime 运行时（Store/Linker/host 导入/trap 收敛）
//! - [`canvas`]：Direct2D/DirectWrite 封装（唯一接触 D2D 绑定的文件）
//! - [`window`]：HWND/DPI/锚点定位/消息分发/动画计时/设备丢失恢复
//! - [`backend`]：ThemeBackend 实现
//!
//! 加载或实例化 .wasm 失败时 `Factory::create` 返回 `Err`，
//! renderer 走既有主题回退路径。
pub use weasel_theme_api as theme_api;
pub use weasel_theme_support::{appearance, bindings, d2d_bindings, presentation};

pub mod backend;
pub mod canvas;
mod composition;
mod data;
mod geometry;
mod glass;
pub mod protocol;
pub mod runtime;
pub mod window;

#[path = "mod.rs"]
mod theme;

weasel_theme_api::export_theme!(theme::Factory);
