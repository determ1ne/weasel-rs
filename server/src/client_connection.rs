//! 为通用 RPC 连接维护输入活跃度、布局缓存和连接回收所需的状态。
//!
//! RPC 收发仍由 [`RpcConnection`] 承担；本模块用短时互斥锁保护输入状态与有界布局队列，
//! 并在回收决策时结合输入优先级和传输层候选信息。

use std::{collections::VecDeque, ops::Deref, sync::Mutex, time::Instant};

use weasel_common::{
    message::{ContextToken, LayoutUpdate},
    rpc::{RetireCandidate, RpcConnection},
};

const LAYOUT_CAPACITY: usize = 32;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
/// 输入连接的回收优先级，枚举顺序即从最适合回收到最不适合回收。
enum InputPriority {
    /// 尚未聚焦且没有组合输入。
    Inactive,
    /// 正在组合输入，但当前未聚焦。
    Composing,
    /// 当前拥有焦点，应优先保留。
    Focused,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
/// 输入层与传输层共同生成的一次回收候选快照。
///
/// 排序先按输入优先级，再按最近请求时间和传输候选排序；执行回收时仍需验证快照未过期。
pub(crate) struct InputRetireCandidate {
    /// 生成候选时的输入优先级。
    priority: InputPriority,
    /// 最近一次传输请求的时间，用于优先回收较久未活动的连接。
    last_request: Instant,
    /// 传输层用于验证并执行回收的候选凭据。
    transport: RetireCandidate,
}

#[derive(Default)]
/// 从引擎汇总的连接级输入状态。
struct InputState {
    /// 是否已由引擎登记；未登记连接不参加输入会话回收排序。
    registered: bool,
    /// 连接是否至少有一个上下文处于焦点状态。
    focused: bool,
    /// 连接是否至少有一个上下文正在组合输入。
    composing: bool,
}

/// `weasel-server` 拥有的输入连接；通用收发由内部 [`RpcConnection`] 完成。
///
/// 同一连接可维护多个上下文的布局更新。布局缓存有固定容量，并按上下文合并更新，
/// 因而高频光标移动不会无限积压，也不会让同一上下文的旧位置覆盖最新位置。
pub(crate) struct ClientConnection {
    /// 底层 RPC 生命周期、发送队列和连接活跃度。
    rpc: RpcConnection,
    /// 引擎汇总的连接级输入状态。
    input: Mutex<InputState>,
    /// 按上下文保留的有限布局更新队列。
    layouts: Mutex<VecDeque<LayoutUpdate>>,
}

impl ClientConnection {
    /// 包装 RPC 连接并初始化输入状态与布局缓存。
    pub(crate) fn new(rpc: RpcConnection) -> Self {
        Self {
            rpc,
            input: Mutex::new(InputState::default()),
            layouts: Mutex::new(VecDeque::new()),
        }
    }

    /// 合并同一上下文的布局更新并追加到有界队列。
    ///
    /// 队列满时淘汰最早的一项；锁仅覆盖队列替换，不跨越 RPC 或引擎调用。
    pub(crate) fn push_layout(&self, update: LayoutUpdate) {
        let key = context_id(&update);
        let mut layouts = self
            .layouts
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        layouts.retain(|item| context_id(item) != key);
        if layouts.len() >= LAYOUT_CAPACITY {
            layouts.pop_front();
        }
        layouts.push_back(update);
    }

    /// 取出与令牌完全匹配的最早一项布局更新。
    ///
    /// 不匹配项保留供对应会话读取；队列锁保护查找和移除这一原子操作。
    pub(crate) fn take_layout_for(&self, token: Option<&ContextToken>) -> Option<LayoutUpdate> {
        let mut layouts = self
            .layouts
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let index = layouts
            .iter()
            .position(|update| update.token.as_ref() == token)?;
        layouts.remove(index)
    }

    /// 删除指定上下文的全部布局更新；不带令牌的更新也会被清除。
    pub(crate) fn forget_layout(&self, context_id: u64) {
        self.layouts
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .retain(|update| {
                update
                    .token
                    .as_ref()
                    .is_none_or(|token| token.context_id != context_id)
            });
    }

    /// 更新连接级焦点和组合状态，并同步底层连接的空闲标记。
    ///
    /// 即使连接同时承载多个输入上下文，也应由引擎先汇总为连接级布尔值再调用。
    pub(crate) fn set_input_state(&self, focused: bool, composing: bool) {
        let mut input = self
            .input
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        input.registered = true;
        input.focused = focused;
        input.composing = composing;
        self.rpc.set_idle(!focused && !composing);
    }

    /// 获取当前可回收候选；未登记或传输层不允许回收时返回 `None`。
    ///
    /// 返回值包含状态快照，后续执行时会重新校验优先级，避免依据过时活跃度回收连接。
    pub(crate) fn reclaim_candidate(&self) -> Option<InputRetireCandidate> {
        let input = self
            .input
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if !input.registered {
            return None;
        }
        let transport = self.rpc.retire_candidate()?;
        Some(InputRetireCandidate {
            priority: priority(&input),
            last_request: transport.last_request,
            transport,
        })
    }

    /// 仅当输入优先级仍与候选快照一致时尝试传输层回收。
    ///
    /// 状态变化或传输层凭据失效时返回 `false`，调用者应重新枚举候选。
    pub(crate) fn reclaim(&self, expected: InputRetireCandidate) -> bool {
        let input = self
            .input
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if !input.registered || priority(&input) != expected.priority {
            return false;
        }
        self.rpc.retire(expected.transport)
    }
}

impl Deref for ClientConnection {
    type Target = RpcConnection;

    /// 暴露底层 RPC 连接接口，避免重复转发通用传输方法。
    fn deref(&self) -> &Self::Target {
        &self.rpc
    }
}

/// 按焦点优先、组合次之、空闲最后计算回收等级。
fn priority(state: &InputState) -> InputPriority {
    if state.focused {
        InputPriority::Focused
    } else if state.composing {
        InputPriority::Composing
    } else {
        InputPriority::Inactive
    }
}

/// 取布局更新的上下文标识；缺少令牌时使用零作为合并键。
fn context_id(update: &LayoutUpdate) -> u64 {
    update.token.as_ref().map_or(0, |token| token.context_id)
}
