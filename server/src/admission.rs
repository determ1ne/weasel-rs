//! Input-intent admission with ranked reclamation; never truncate in-flight work.
use std::{collections::HashMap, sync::Arc, time::Duration};
use weasel_common::rpc::RpcConnection;

/// Four extra connections may perform a handshake/OpenInput before presenting
/// real input intent. They cannot take an active slot just by connecting.
pub(crate) const PENDING_LIMIT: usize = 4;
pub(crate) struct Gate {
    slots: Arc<tokio::sync::Semaphore>,
    pub peers: std::sync::Mutex<HashMap<u64, Arc<RpcConnection>>>,
    pub changed: tokio::sync::Notify,
    active: std::sync::Mutex<std::collections::HashSet<u64>>,
    pending: std::sync::atomic::AtomicUsize,
}
pub(crate) struct Admission {
    id: u64,
    gate: Arc<Gate>,
    permit: Option<tokio::sync::OwnedSemaphorePermit>,
    pub deadline: tokio::time::Instant,
}
impl Gate {
    pub fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            slots: Arc::new(tokio::sync::Semaphore::new(limit)),
            peers: Default::default(),
            changed: Default::default(),
            active: Default::default(),
            pending: Default::default(),
        })
    }
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
    pub fn pending(&self) -> bool {
        self.permit.is_none()
    }
    pub fn notifier(&self, engine: Arc<dyn Fn() + Send + Sync>) -> Arc<dyn Fn() + Send + Sync> {
        let gate = Arc::downgrade(&self.gate);
        Arc::new(move || {
            engine();
            if let Some(gate) = gate.upgrade() {
                gate.changed.notify_waiters();
            }
        })
    }
    pub async fn promote(&mut self, id: u64, connection: &RpcConnection) -> bool {
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
fn retire_stale(connections: &HashMap<u64, Arc<RpcConnection>>, age: Duration) -> Option<u64> {
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
            let old = Arc::new(listener.accept().await.unwrap());
            let new_client = RpcClient::connect(&pipe).await.unwrap();
            let new = Arc::new(listener.accept().await.unwrap());
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
            let a = Arc::new(listener.accept().await.unwrap());
            let second = RpcClient::connect(&pipe).await.unwrap();
            let b = Arc::new(listener.accept().await.unwrap());
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
