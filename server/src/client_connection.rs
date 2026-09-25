//! 输入服务对通用 RPC 连接附加的领域状态。

use std::{collections::VecDeque, ops::Deref, sync::Mutex, time::Instant};

use weasel_common::{
    message::{ContextToken, LayoutUpdate},
    rpc::{RetireCandidate, RpcConnection},
};

const LAYOUT_CAPACITY: usize = 32;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum InputPriority {
    Inactive,
    Composing,
    Focused,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct InputRetireCandidate {
    priority: InputPriority,
    last_request: Instant,
    transport: RetireCandidate,
}

#[derive(Default)]
struct InputState {
    registered: bool,
    focused: bool,
    composing: bool,
}

/// `weasel-server` 拥有的输入连接；通用收发由内部 [`RpcConnection`] 完成。
pub(crate) struct ClientConnection {
    rpc: RpcConnection,
    input: Mutex<InputState>,
    layouts: Mutex<VecDeque<LayoutUpdate>>,
}

impl ClientConnection {
    pub(crate) fn new(rpc: RpcConnection) -> Self {
        Self {
            rpc,
            input: Mutex::new(InputState::default()),
            layouts: Mutex::new(VecDeque::new()),
        }
    }

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

    fn deref(&self) -> &Self::Target {
        &self.rpc
    }
}

fn priority(state: &InputState) -> InputPriority {
    if state.focused {
        InputPriority::Focused
    } else if state.composing {
        InputPriority::Composing
    } else {
        InputPriority::Inactive
    }
}

fn context_id(update: &LayoutUpdate) -> u64 {
    update.token.as_ref().map_or(0, |token| token.context_id)
}
