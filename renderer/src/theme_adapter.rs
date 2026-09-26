//! 在 IPC 快照、渲染器事件和主题接口之间执行唯一的转换。
//!
//! 这里保留主题回调创建时的所有者、会话、上下文令牌和修订号，并在入队前
//! 校验动作是否符合该快照，避免主题实现直接接触 IPC 类型或绕过运行时约束。
use crate::state::Owner;
use crate::theme_api::{
    Anchor, CandidateItem, CandidateView, EventSink, ModeIndicator, ModeIndicatorReason, UiAction,
};
use weasel_common::message::{
    ModeIndicatorReason as ProtoModeIndicatorReason, RenderSnapshot, RendererEvent,
    RendererEventAction,
};

/// 将传输层快照复制为主题视图，并附加用于淘汰过期回调的内容代号。
///
/// 文本和候选项会被克隆，因此调用后视图不依赖快照的借用生命周期；调用方
/// 应避免对未变化内容重复转换，以控制分配开销。
pub fn view(snapshot: &RenderSnapshot, content_id: u64) -> CandidateView {
    CandidateView {
        preedit: snapshot
            .preedit
            .as_ref()
            .map(|p| crate::theme_api::Preedit {
                text: p.text.clone(),
                cursor: p.cursor_utf16,
            }),
        content_id,
        active: snapshot.active,
        ascii_mode: snapshot.ascii_mode,
        mode_indicator: snapshot.mode_indicator.as_ref().and_then(|indicator| {
            let reason = match ProtoModeIndicatorReason::try_from(indicator.reason).ok()? {
                ProtoModeIndicatorReason::Focus => ModeIndicatorReason::Focus,
                ProtoModeIndicatorReason::UserSwitch => ModeIndicatorReason::UserSwitch,
                ProtoModeIndicatorReason::Unspecified => return None,
            };
            Some(ModeIndicator {
                id: indicator.id,
                ascii_mode: indicator.ascii_mode,
                reason,
            })
        }),
        visible: snapshot.visible,
        anchor: snapshot.anchor.as_ref().map(|r| Anchor {
            left: r.left,
            top: r.top,
            right: r.right,
            bottom: r.bottom,
            valid: r.valid,
        }),
        items: snapshot
            .items
            .iter()
            .map(|item| CandidateItem {
                primary_text: item.primary_text.clone(),
                secondary_text: item.secondary_text.clone(),
                enabled: item.enabled,
            })
            .collect(),
        selected_index: snapshot.selected_index,
        page_start: snapshot.page_start,
        total_item_count: snapshot.total_item_count,
        can_page_previous: snapshot.can_page_previous,
        can_page_next: snapshot.can_page_next,
    }
}

/// 为一个快照创建带身份的主题事件接收端。
///
/// 禁用候选、不可用的翻页动作和未知动作会被丢弃。发送使用有界 Tokio 队列的
/// 非阻塞 `try_send`；队列满或接收端关闭时事件丢弃，不阻塞 UI 回调线程。
pub fn events(
    owner: Owner,
    snapshot: &RenderSnapshot,
    sender: tokio::sync::mpsc::Sender<(Owner, RendererEvent)>,
) -> EventSink {
    let session_id = snapshot.session_id;
    let token = snapshot.token.clone();
    let revision = snapshot.revision;
    let enabled: Vec<_> = snapshot.items.iter().map(|item| item.enabled).collect();
    let previous = snapshot.can_page_previous;
    let next = snapshot.can_page_next;
    EventSink::new(move |action| {
        let (action, item_index) = match action {
            UiAction::ItemInvoked(i) if enabled.get(i as usize) == Some(&true) => {
                (RendererEventAction::ItemInvoked, i)
            }
            UiAction::NavigatePrevious if previous => (RendererEventAction::NavigatePrevious, 0),
            UiAction::NavigateNext if next => (RendererEventAction::NavigateNext, 0),
            UiAction::Dismiss => (RendererEventAction::Dismiss, 0),
            UiAction::OpenEmojiPanel => (RendererEventAction::OpenEmojiPanel, 0),
            _ => return,
        };
        let _ = sender.try_send((
            owner,
            RendererEvent {
                session_id,
                token: token.clone(),
                revision,
                action: action as i32,
                item_index,
            },
        ));
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use weasel_common::message::{ContextToken, RenderItem};

    #[test]
    fn callbacks_keep_original_identity_and_reject_disabled_actions() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let mut snapshot = RenderSnapshot {
            session_id: 42,
            revision: 7,
            token: Some(ContextToken {
                context_id: 3,
                ..Default::default()
            }),
            items: vec![
                RenderItem {
                    enabled: true,
                    ..Default::default()
                },
                RenderItem::default(),
            ],
            ..Default::default()
        };
        let old = events(1, &snapshot, tx.clone());
        snapshot.session_id = 43;
        snapshot.revision = 8;
        snapshot.token.as_mut().unwrap().context_id = 4;
        let new = events(2, &snapshot, tx);
        old.send(UiAction::ItemInvoked(1));
        old.send(UiAction::ItemInvoked(2));
        old.send(UiAction::NavigateNext);
        old.send(UiAction::NavigatePrevious);
        assert!(rx.try_recv().is_err());
        old.send(UiAction::ItemInvoked(0));
        new.send(UiAction::ItemInvoked(0));
        let (owner, event) = rx.try_recv().unwrap();
        assert_eq!((owner, event.session_id, event.revision), (1, 42, 7));
        assert_eq!(event.token.unwrap().context_id, 3);
        let (owner, event) = rx.try_recv().unwrap();
        assert_eq!((owner, event.session_id, event.revision), (2, 43, 8));
        assert_eq!(event.token.unwrap().context_id, 4);
    }

    #[test]
    fn geometry_changes_do_not_change_content_but_identity_changes_do() {
        let snapshot = RenderSnapshot {
            items: vec![RenderItem::default()],
            ..Default::default()
        };
        let first = view(&snapshot, 1);
        let mut moved = first.clone();
        moved.anchor = Some(Anchor {
            left: 100,
            valid: true,
            ..Default::default()
        });
        assert!(crate::theme_api::same_content(&first, &moved));
        moved.content_id = 2;
        assert!(!crate::theme_api::same_content(&first, &moved));
        moved.content_id = 1;
        moved.items[0].primary_text = "changed".into();
        assert!(!crate::theme_api::same_content(&first, &moved));
    }
}
