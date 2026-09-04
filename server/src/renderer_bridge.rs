//! The engine publishes into a single latest-value slot, never into a network queue.
use crate::engine::Work;
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::SyncSender,
    },
    time::Duration,
};
use tokio::sync::watch;
use weasel_common::{
    message::{RenderItem, RenderRect, RenderSnapshot},
    rpc::{RpcClient, default_renderer_pipe_name},
};

#[derive(Clone)]
pub(crate) struct RendererPublisher {
    snapshots: watch::Sender<Option<RenderSnapshot>>,
    sequence: Arc<AtomicU64>,
}
impl RendererPublisher {
    pub fn channel() -> (Self, watch::Receiver<Option<RenderSnapshot>>) {
        let (tx, rx) = watch::channel(None);
        (
            Self {
                snapshots: tx,
                sequence: Arc::new(AtomicU64::new(0)),
            },
            rx,
        )
    }
    pub fn publish(&self, mut snapshot: RenderSnapshot) {
        // Allocate while holding the watch slot's write lock so concurrent clones
        // cannot publish an older sequence after a newer one. Never wrap to zero.
        self.snapshots.send_modify(|latest| {
            snapshot.sequence = self
                .sequence
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                    value.checked_add(1)
                })
                .expect("renderer publication sequence exhausted")
                + 1;
            *latest = Some(snapshot);
        });
    }
}

pub(crate) fn spawn(
    mut snapshots: watch::Receiver<Option<RenderSnapshot>>,
    engine: SyncSender<Work>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            if snapshots.borrow().is_none() && snapshots.changed().await.is_err() {
                return;
            }
            let client = match RpcClient::connect_as(
                default_renderer_pipe_name(),
                weasel_common::message::PeerRole::Server,
            )
            .await
            {
                Ok(client) => client,
                Err(_) => {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    continue;
                }
            };
            let mut events = client.subscribe_renderer_events();
            let mut dirty = true;
            // One connection/task owns both directions; reconnect drops its subscription.
            loop {
                let latest = if dirty {
                    snapshots.borrow_and_update().clone()
                } else {
                    None
                };
                dirty = false;
                if let Some(snapshot) = latest {
                    if !matches!(
                        tokio::time::timeout(
                            Duration::from_millis(250),
                            client.send_render_snapshot(snapshot)
                        )
                        .await,
                        Ok(Ok(()))
                    ) {
                        break;
                    }
                }
                tokio::select! {
                    changed = snapshots.changed() => { if changed.is_err() { client.disconnect().await; return; } dirty = true; }
                    event = events.recv() => match event {
                        Ok(event) => { let _ = engine.try_send(Work::Renderer(event)); }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => (),
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    },
                    _ = tokio::time::sleep(Duration::from_millis(250)) => {
                        if !client.is_connected() { break; }
                    }
                }
            }
            client.disconnect().await;
        }
    })
}

pub(crate) fn render_snapshot(
    session_id: u64,
    revision: u64,
    response: &weasel_common::message::KeyEventResponse,
    anchor: &RenderRect,
) -> RenderSnapshot {
    RenderSnapshot {
        sequence: 0, // Assigned only when published; independent of input revision.
        session_id,
        revision,
        visible: !response.candidates.is_empty() && anchor.valid,
        anchor: Some(anchor.clone()),
        items: response
            .candidates
            .iter()
            .map(|candidate| RenderItem {
                primary_text: candidate.text.clone(),
                secondary_text: candidate.comment.clone(),
                enabled: true,
                kind: "candidate".to_owned(),
            })
            .collect(),
        selected_index: response.selected_candidate,
        page_start: response.page_start,
        total_item_count: None,
        can_page_previous: response.can_page_previous,
        can_page_next: response.can_page_next,
        token: response.token.clone(),
    }
}

#[cfg(test)]
mod render_tests {
    use super::*;

    #[test]
    fn clones_share_sequence_independent_of_context_and_input_revision() {
        let (publisher, mut receiver) = RendererPublisher::channel();
        let clone = publisher.clone();
        for (index, sender) in [&publisher, &clone, &publisher].into_iter().enumerate() {
            sender.publish(RenderSnapshot {
                sequence: 999, // Caller values are always overwritten.
                session_id: index as u64 + 1,
                revision: 7,
                visible: index != 2,
                ..Default::default()
            });
            let latest = receiver.borrow_and_update();
            let latest = latest.as_ref().unwrap();
            assert_eq!(latest.sequence, index as u64 + 1);
            assert_eq!(latest.revision, 7);
        }
    }

    #[test]
    fn concurrent_clones_cannot_replace_latest_with_an_older_sequence() {
        let (publisher, receiver) = RendererPublisher::channel();
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let publisher = publisher.clone();
                scope.spawn(move || {
                    for _ in 0..100 {
                        publisher.publish(RenderSnapshot::default());
                    }
                });
            }
        });
        assert_eq!(receiver.borrow().as_ref().unwrap().sequence, 400);
    }

    #[test]
    fn stalled_renderer_retains_only_latest_snapshot_including_hide() {
        let (publisher, mut receiver) = RendererPublisher::channel();
        for revision in 1..=10000 {
            publisher.publish(RenderSnapshot {
                revision,
                visible: true,
                ..Default::default()
            });
        }
        assert_eq!(
            receiver.borrow_and_update().as_ref().unwrap().revision,
            10000
        );
        publisher.publish(RenderSnapshot {
            revision: 10001,
            visible: false,
            ..Default::default()
        });
        let latest = receiver.borrow_and_update();
        assert_eq!(latest.as_ref().unwrap().revision, 10001);
        assert!(!latest.as_ref().unwrap().visible);
    }

    #[test]
    fn snapshot_preserves_engine_page_and_all_candidates() {
        let response = weasel_common::message::KeyEventResponse {
            page_start: 7,
            can_page_previous: true,
            can_page_next: true,
            selected_candidate: 5,
            candidates: (0..7)
                .map(|i| weasel_common::message::Candidate {
                    text: i.to_string(),
                    comment: String::new(),
                })
                .collect(),
            ..Default::default()
        };
        let snapshot = render_snapshot(
            42,
            3,
            &response,
            &RenderRect {
                valid: true,
                ..Default::default()
            },
        );
        assert_eq!(snapshot.items.len(), 7);
        assert_eq!(snapshot.page_start, 7);
        assert_eq!(snapshot.selected_index, 5);
        assert!(snapshot.can_page_previous && snapshot.can_page_next);
        assert_eq!(snapshot.total_item_count, None);
        assert_eq!(snapshot.sequence, 0);
    }
}
