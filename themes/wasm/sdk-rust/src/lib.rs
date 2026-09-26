//! 面向 WASM 主题作者的 Rust SDK。优先从 `lifecycle`、`config`、`view`、`draw`、
//! `surface`、`interaction` 等分组模块导入；`raw` 仅用于需要直接调用宿主 ABI 的高级场景。
//! 主题应先在创建入口读取并缓存配置和资源，再在事件入口读取快照、构建完整画面，
//! 并以 `FrameResult::Present` 提交；资源句柄由 RAII 类型持有并在离开作用域时释放。
//!
//! 默认绘制目标是 `Surface::primary()`。复杂主题可以创建由 host 管理的辅助 Surface，
//! 在一个 WASM 实例内共享配置、资源和动画状态；选择目标后，现有绘制与窗口属性 API
//! 都作用于该 Surface。一次 `Present` 原子提交本事件触碰的所有 Surface，guest 不会
//! 接触 HWND、DPI 或原生窗口生命周期。
pub mod types;
pub use types::{
    ABI_VERSION, Action, Capability, ConfigScope, DataKind as Kind, ErrorCode, EventKind,
    FrameResult, Mode, PointerPhase, SurfaceKind,
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

/// 模块级主题选项；JSON Pointer 路径由 [`config::Data`] 查询。
pub use config::{Data, OPTIONS, SETTINGS};
/// 普通诊断日志和需要用户处理的问题通知。
pub use diagnostics::{log, report_notice};
/// 常用绘制和文本测量入口；新主题可从 [`draw`] 分组导入。
pub use draw::*;
/// 指针命中、语义动作和固定窗口拖动入口。
pub use interaction::{begin_drag, hit_region, pointer_region, send_action};
/// 由 `Drop` 管理的字体、文本布局和图片资源。
pub use resources::{Font, Image, TextLayout};
/// 展示表面尺寸、材质和定位设置。
pub use surface::*;
/// 当前事件中的只读视图快照。
pub use view::View;
/// 请求一次后续动画事件；通常优先使用 [`animation::request_frame`]。
pub fn request_frame() {
    unsafe {
        raw::request_frame();
    }
}
/// 读取宿主单调时钟的绝对毫秒值，与事件的 `now` 使用同一时钟域。
pub fn time_ms() -> f64 {
    unsafe { raw::time_ms() }
}
