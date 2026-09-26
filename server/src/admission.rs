//! 按真实输入意图准入连接，并在槽位不足时按优先级回收可淘汰连接。
//!
//! 握手本身只占用有界待准入名额；连接提交输入意图后才竞争活动槽位。回收不会截断
//! 正在执行的请求，活动槽位由信号量许可持有至准入对象销毁。
use crate::client_connection::ClientConnection;
use std::{collections::HashMap, sync::Arc, time::Duration};

/// 允许先完成握手或打开输入上下文、再提交真实输入意图的额外连接数。
///
/// 超出此上限时 [`Gate::start`] 返回 `None`；仅建立连接不会占用活动槽位。
pub(crate) const PENDING_LIMIT: usize = 4;
/// 管理活动连接槽位、连接索引以及等待准入者的唤醒通知。
pub(crate) struct Gate {
    /// 活动输入连接的并发上限。
    slots: Arc<tokio::sync::Semaphore>,
    /// 按连接标识索引的服务端连接，供准入回收候选。
    pub peers: std::sync::Mutex<HashMap<u64, Arc<ClientConnection>>>,
    /// 连接活跃状态改变或槽位释放时唤醒等待者。
    pub changed: tokio::sync::Notify,
    /// 当前持有活动槽位的连接标识。
    active: std::sync::Mutex<std::collections::HashSet<u64>>,
    /// 已开始但尚未取得活动许可的连接数，受 [`PENDING_LIMIT`] 限制。
    pending: std::sync::atomic::AtomicUsize,
}
/// 单条连接的准入状态；持有的许可代表其占用一个活动槽位。
pub(crate) struct Admission {
    /// 在 [`Gate::active`] 中登记的连接标识。
    id: u64,
    /// 共享的准入门及其槽位、连接索引。
    gate: Arc<Gate>,
    /// `Some` 时连接已准入；释放该许可会向其他等待者开放槽位。
    permit: Option<tokio::sync::OwnedSemaphorePermit>,
    /// 等待真实输入意图取得活动槽位的绝对截止时间。
    pub deadline: tokio::time::Instant,
}
impl Gate {
    /// 创建具有指定活动连接上限的共享准入门。
    pub fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            slots: Arc::new(tokio::sync::Semaphore::new(limit)),
            peers: Default::default(),
            changed: Default::default(),
            active: Default::default(),
            pending: Default::default(),
        })
    }
    /// 登记一条待准入连接；待处理名额耗尽时立即返回 `None`。
    ///
    /// 成功登记的对象有两秒准入期限；成功准入或未准入对象销毁时都会释放待处理计数。
    pub fn start(self: &Arc<Self>, id: u64) -> Option<Admission> {
        use std::sync::atomic::Ordering;
        self.pending
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < PENDING_LIMIT).then_some(n + 1)
            })
            .ok()?;
        Some(Admission {
            id,
            gate: self.clone(),
            permit: None,
            deadline: tokio::time::Instant::now() + Duration::from_secs(2),
        })
    }
}
impl Admission {
    /// 当前尚未取得活动槽位时返回 `true`。
    pub fn pending(&self) -> bool {
        self.permit.is_none()
    }
    /// 组合引擎唤醒回调与准入等待者通知。
    ///
    /// 回调先调用引擎，再通过弱引用通知仍存活的准入门，避免通知闭包延长门的生命周期。
    pub fn notifier(&self, engine: Arc<dyn Fn() + Send + Sync>) -> Arc<dyn Fn() + Send + Sync> {
        let gate = Arc::downgrade(&self.gate);
        Arc::new(move || {
            engine();
            if let Some(gate) = gate.upgrade() {
                gate.changed.notify_waiters();
            }
        })
    }
    /// 在截止时间内取得活动许可，必要时尝试回收较低优先级的活动连接。
    ///
    /// 已准入时幂等返回 `true`；等待期间若连接断开、超时或信号量关闭则返回 `false`。
    /// 只有成功持有许可后才登记为活动连接并减少待处理计数。
    pub async fn promote(&mut self, id: u64, connection: &ClientConnection) -> bool {
        if self.permit.is_some() {
            return true;
        }
        let result = tokio::time::timeout_at(self.deadline, async {
            loop {
                let changed = self.gate.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                if let Ok(permit) = self.gate.slots.clone().try_acquire_owned() {
                    return Some(permit);
                }
                let retired = {
                    let active = self
                        .gate
                        .active
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .clone();
                    let peers = self.gate.peers.lock().unwrap_or_else(|p| p.into_inner());
                    let mut candidates: Vec<_> = peers
                        .iter()
                        .filter(|(peer, _)| **peer != id && active.contains(peer))
                        .filter_map(|(peer, c)| c.reclaim_candidate().map(|rank| (rank, *peer)))
                        .collect();
                    candidates.sort_unstable();
                    candidates
                        .into_iter()
                        .any(|(rank, peer)| peers[&peer].reclaim(rank))
                };
                tokio::select! {
                    permit = self.gate.slots.clone().acquire_owned() => return permit.ok(),
                    _ = connection.disconnected() => return None,
                    _ = changed, if !retired => {},
                }
            }
        })
        .await;
        self.permit = result.ok().flatten();
        if self.permit.is_some() {
            self.gate
                .active
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .insert(id);
            self.gate
                .pending
                .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
        }
        self.permit.is_some()
    }
}
impl Drop for Admission {
    /// 清理活动登记并释放许可；尚未准入的对象同时归还待处理名额。
    fn drop(&mut self) {
        if self.permit.is_none() {
            self.gate
                .pending
                .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
        }
        self.gate
            .active
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&self.id);
        self.permit.take();
        self.gate.changed.notify_waiters();
    }
}

#[cfg(test)]
/// 从连接集合中按最早空闲时间尝试淘汰一条超过给定空闲时长的连接。
fn retire_stale(connections: &HashMap<u64, Arc<ClientConnection>>, age: Duration) -> Option<u64> {
    let mut candidates: Vec<_> = connections
        .iter()
        .filter_map(|(id, connection)| connection.stale_since(age).map(|since| (since, *id)))
        .collect();
    candidates.sort_unstable();
    candidates
        .into_iter()
        .find_map(|(_, id)| connections[&id].evict_if_stale(age).then_some(id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use weasel_common::rpc::{RpcClient, RpcServer};
    #[test]
    fn pending_area_is_bounded_and_drop_releases_capacity() {
        let gate = Gate::new(128);
        let mut pending: Vec<_> = (0..PENDING_LIMIT)
            .map(|id| gate.start(id as u64).unwrap())
            .collect();
        assert!(gate.start(99).is_none());
        pending.pop();
        assert!(gate.start(99).is_some());
    }
    #[tokio::test]
    async fn pending_input_reclaims_composition_and_waits_for_reader_slot_release() {
        tokio::time::timeout(Duration::from_secs(5), async {
            let pipe = format!(r"\\.\pipe\weasel-reclaim-{}", std::process::id());
            let listener = RpcServer::new(&pipe);
            listener.bind().await.unwrap();
            let old_client = RpcClient::connect(&pipe).await.unwrap();
            let old = Arc::new(ClientConnection::new(listener.accept().await.unwrap()));
            let new_client = RpcClient::connect(&pipe).await.unwrap();
            let new = Arc::new(ClientConnection::new(listener.accept().await.unwrap()));
            let gate = Gate::new(1);
            gate.peers
                .lock()
                .unwrap()
                .extend([(1, old.clone()), (2, new.clone())]);
            let mut owner = gate.start(1).unwrap();
            assert!(owner.promote(1, &old).await);
            old.set_input_state(true, true);
            let mut pending = gate.start(2).unwrap();
            assert!(pending.pending());
            assert!(old_client.is_connected()); // handshake alone never evicts
            let release = tokio::spawn(async move {
                old.disconnected().await;
                drop(owner);
            });
            assert!(pending.promote(2, &new).await);
            release.await.unwrap();
            old_client.disconnected().await;
            assert!(new_client.is_connected());
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn oldest_idle_connection_is_retired_but_active_peer_is_kept() {
        tokio::time::timeout(Duration::from_secs(5), async {
            let pipe = format!(r"\\.\pipe\weasel-admission-{}", std::process::id());
            let listener = RpcServer::new(&pipe);
            listener.bind().await.unwrap();
            let first = RpcClient::connect(&pipe).await.unwrap();
            let a = Arc::new(ClientConnection::new(listener.accept().await.unwrap()));
            let second = RpcClient::connect(&pipe).await.unwrap();
            let b = Arc::new(ClientConnection::new(listener.accept().await.unwrap()));
            a.set_idle(true);
            b.set_idle(true);
            let connections = HashMap::from([(1, a), (2, b.clone())]);
            assert_eq!(retire_stale(&connections, Duration::ZERO), Some(1));
            first.disconnected().await;
            b.set_idle(false);
            assert_eq!(retire_stale(&connections, Duration::ZERO), None);
            assert!(second.is_connected());
        })
        .await
        .unwrap();
    }
}
