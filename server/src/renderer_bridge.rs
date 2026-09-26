//! 在引擎与渲染器之间传递最新渲染快照，并将渲染器事件送回引擎。
//!
//! 快照保存在容量为一的 watch 通道中，慢速或暂时离线的渲染器只会收到最新状态，
//! 不会积压过时网络消息。渲染器能力协商结果通过发布器共享给引擎。
use crate::engine::Work;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::watch;
use weasel_common::{
    message::{QueryConfig, RenderItem, RenderRect, RenderSnapshot, envelope::Payload},
    rpc::{RpcClient, RpcError, default_renderer_pipe_name},
};

/// 查询渲染器是否支持外置预编辑区。
///
/// 只接受配置响应中严格等于 JSON `true` 的值；其他响应类型视为 RPC 协议错误。
async fn query_preedit_capability(client: &RpcClient) -> Result<bool, RpcError> {
    let response = client
        .request(Payload::QueryConfig(QueryConfig {
            refresh: false,
            path: ".capabilities.preedit".into(),
        }))
        .await?;
    match response.payload {
        Some(Payload::ConfigValue(value)) => Ok(value.json.as_deref() == Some("true")),
        _ => Err(RpcError::UnexpectedResponse),
    }
}

#[derive(Clone)]
/// 在线程间共享的渲染状态发布端。
///
/// 克隆句柄共享能力标志、最新快照槽和单调序号；写入采用覆盖语义，不为慢接收方
/// 保存历史快照。
pub(crate) struct RendererPublisher {
    /// 当前渲染器是否支持外置预编辑区，供引擎以 Acquire/Release 顺序读取。
    preedit: Arc<AtomicBool>,
    /// 容量为一的最新快照槽；所有发布者克隆共享同一通道。
    snapshots: watch::Sender<Option<RenderSnapshot>>,
    /// 为所有发布操作分配唯一递增序号，达到上限时拒绝继续发布。
    sequence: Arc<AtomicU64>,
}
impl RendererPublisher {
    /// 创建发布器及其唯一订阅端；订阅端初始尚无快照。
    pub fn channel() -> (Self, watch::Receiver<Option<RenderSnapshot>>) {
        let (tx, rx) = watch::channel(None);
        (
            Self {
                preedit: Arc::new(AtomicBool::new(false)),
                snapshots: tx,
                sequence: Arc::new(AtomicU64::new(0)),
            },
            rx,
        )
    }
    /// 覆盖发布最新快照，并由发布器统一分配序号。
    ///
    /// 序号分配与 watch 槽更新在同一写锁内完成，以保证并发克隆发布后，槽中的快照
    /// 不会出现序号倒退；`u64` 耗尽时触发 panic，避免序号回绕产生歧义。
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
    /// 返回最近一次完成的渲染器能力协商结果。
    pub fn supports_preedit(&self) -> bool {
        self.preedit.load(Ordering::Acquire)
    }
}

/// 启动渲染器桥接任务。
///
/// `eager` 为真时连接与能力查询立即开始；否则仅在存在可见快照后连接。任务负责
/// 重连、双向转发和更新能力标志；调用方可取消并等待返回的句柄以完成清理。
pub(crate) fn spawn(
    mut snapshots: watch::Receiver<Option<RenderSnapshot>>,
    engine: crate::worker::Sender<Work>,
    publisher: RendererPublisher,
    eager: bool,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            // External preedit needs capability negotiation before the first key.
            // Inline-only sessions retain the lazy, visible-snapshot connection.
            if !eager && snapshots.borrow().as_ref().is_none_or(|s| !s.visible) {
                if snapshots.changed().await.is_err() {
                    return;
                }
                continue;
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
            let supported = match tokio::time::timeout(
                Duration::from_secs(2),
                query_preedit_capability(&client),
            )
            .await
            {
                Ok(Ok(value)) => value,
                _ => false,
            };
            publisher.preedit.store(supported, Ordering::Release);
            (engine.notifier())();
            let mut events = client.subscribe();
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
                    if client.publish(Payload::RenderSnapshot(snapshot)).is_err() {
                        break;
                    }
                }
                tokio::select! {
                    changed = snapshots.changed() => { if changed.is_err() { client.disconnect().await; return; } dirty = true; }
                    event = events.recv() => match event {
                        Ok(envelope) => {
                            if let Some(Payload::RendererEvent(event)) = envelope.payload {
                                let _ = engine.try_send(Work::Renderer(event));
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => (),
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    },
                    _ = client.disconnected() => break,
                }
            }
            publisher.preedit.store(false, Ordering::Release);
            (engine.notifier())();
            client.disconnect().await;
        }
    })
}

/// 根据一次引擎按键响应构造渲染快照。
///
/// 快照序号保留为零，由 [`RendererPublisher::publish`] 在真正发布时分配。候选项和
/// 分页状态沿用引擎响应；候选窗只有在存在候选项或外置预编辑内容且锚点有效时可见。
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
        active: true,
        ascii_mode: response.ascii_mode,
        visible: (!response.candidates.is_empty()
            || (response.external_preedit && response.composing))
            && anchor.valid,
        preedit: (response.external_preedit && response.composing).then(|| {
            weasel_common::message::RenderPreedit {
                text: response.composition.clone(),
                cursor_utf16: response.composition_cursor,
            }
        }),
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
