//! 主题 ABI 生命周期使用的版本、能力、事件和返回类型。
//!
//! 宿主先独立查询 ABI 版本与能力，再调用 `theme_create`，随后多次调用
//! `theme_event`；`theme_destroy` 可选且可能在创建失败后调用。查询阶段和模块顶层
//! 初始化不得调用宿主接口。事件修改只有返回 `Present` 才提交，`Keep` 会丢弃展示修改。
pub use crate::types::{ABI_VERSION, Capability, ErrorCode, EventKind, FrameResult, Mode};
