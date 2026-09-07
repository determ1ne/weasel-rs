//! The only translation between wire snapshots/events and the theme contract.
use crate::state::Owner;
use crate::theme_api::{Anchor, CandidateItem, CandidateView, EventSink, UiAction};
use weasel_common::message::{RenderSnapshot, RendererEvent, RendererEventAction};

pub fn view(snapshot: &RenderSnapshot, content_id: u64) -> CandidateView {
    CandidateView {
        preedit: None,
        content_id,
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
