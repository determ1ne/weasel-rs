//! 绑定输入会话与宿主上下文，并校验发往 Rime 或来自渲染器的操作。
//!
//! 路由以连接代次、上下文标识和单调不减的上下文代次识别有效令牌。只有焦点命令可以
//! 将已绑定会话迁移到新上下文；渲染器事件还必须对应当前焦点和会话修订号。
use weasel_common::message::{ContextAction, ContextToken, RendererEvent};

#[derive(Default)]
/// 限定某个 Rime 会话可接受的输入上下文和界面事件。
pub(crate) struct SessionRoute {
    /// 当前绑定的宿主上下文令牌；首次有效输入或合法焦点迁移时建立或更新。
    pub token: Option<ContextToken>,
    /// 路由是否拥有焦点；失焦时渲染器操作一律不被接受。
    pub focused: bool,
}

impl SessionRoute {
    /// 校验令牌并绑定或推进当前上下文代次。
    ///
    /// 首次绑定后，上下文标识和连接代次不可变，代次不可倒退；无令牌、零字段或身份
    /// 不匹配时返回 `false` 且保留当前绑定。成功时保存传入令牌。
    pub fn observe(&mut self, token: Option<&ContextToken>) -> bool {
        if !token.is_some_and(|token| {
            token.context_id != 0 && token.connection_epoch != 0 && token.generation != 0
        }) {
            return false;
        }
        match (&self.token, token) {
            (None, Some(token)) if token.context_id != 0 && token.connection_epoch != 0 => {
                self.token = Some(token.clone());
                true
            }
            (Some(current), Some(next))
                if current.context_id == next.context_id
                    && current.connection_epoch == next.connection_epoch
                    && next.generation >= current.generation =>
            {
                self.token = Some(next.clone());
                true
            }
            _ => false,
        }
    }

    /// 按命令类型校验上下文；仅 `Focus` 可以切换上下文标识。
    ///
    /// 焦点迁移仍要求连接代次相同且上下文代次严格有效并不倒退。其余命令遵循
    /// [`Self::observe`] 的同上下文规则，失败时不改变路由。
    pub fn observe_command(&mut self, token: Option<&ContextToken>, action: ContextAction) -> bool {
        if action == ContextAction::Focus {
            if let (Some(current), Some(next)) = (&self.token, token) {
                if next.context_id != 0
                    && current.connection_epoch == next.connection_epoch
                    && next.generation >= current.generation
                    && next.generation != 0
                {
                    self.token = Some(next.clone());
                    return true;
                }
            }
        }
        self.observe(token)
    }

    /// 判断渲染器事件是否属于当前聚焦上下文且仍对应最新会话修订号。
    pub fn accepts_ui(&self, event: &RendererEvent, revision: u64) -> bool {
        self.focused && self.token == event.token && event.revision == revision
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_focus_can_rebind_context_and_missing_token_is_rejected() {
        let mut route = SessionRoute::default();
        assert!(!route.observe(None));
        let first = ContextToken {
            context_id: 1,
            connection_epoch: 2,
            generation: 1,
        };
        assert!(route.observe(Some(&first)));
        let next = ContextToken {
            context_id: 3,
            generation: 2,
            ..first.clone()
        };
        assert!(!route.observe(Some(&next)));
        assert!(!route.observe_command(Some(&next), ContextAction::Submit));
        assert!(route.observe_command(Some(&next), ContextAction::Focus));
        assert!(!route.observe_command(Some(&first), ContextAction::Focus));
        assert_eq!(route.token, Some(next));
    }
    #[test]
    fn bound_pipe_cannot_switch_context_or_epoch_or_go_backwards() {
        let mut route = SessionRoute::default();
        let token = ContextToken {
            context_id: 9,
            connection_epoch: 3,
            generation: 1,
        };
        assert!(route.observe(Some(&token)));
        assert!(!route.observe(None));
        assert!(!route.observe(Some(&ContextToken {
            context_id: 8,
            ..token.clone()
        })));
        assert!(!route.observe(Some(&ContextToken {
            connection_epoch: 2,
            ..token.clone()
        })));
        assert!(route.observe(Some(&ContextToken {
            generation: 2,
            ..token.clone()
        })));
        assert!(!route.observe(Some(&token)));
    }
    #[test]
    fn stale_click_or_hidden_session_cannot_select_candidate() {
        let mut route = SessionRoute {
            focused: true,
            ..Default::default()
        };
        let mut event = RendererEvent {
            revision: 5,
            ..Default::default()
        };
        assert!(route.accepts_ui(&event, 5));
        event.revision = 4;
        assert!(!route.accepts_ui(&event, 5));
        event.revision = 5;
        route.focused = false;
        assert!(!route.accepts_ui(&event, 5));
    }
}
