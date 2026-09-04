//! Validate session ownership before touching Rime or accepting UI actions.
use weasel_common::message::{ContextAction, ContextToken, RendererEvent};

#[derive(Default)]
pub(crate) struct SessionRoute {
    pub token: Option<ContextToken>,
    pub focused: bool,
}

impl SessionRoute {
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
