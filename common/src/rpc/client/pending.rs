//! 客户端请求 ID 与一次性响应通道的生命周期管理。

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use tokio::sync::oneshot;

use crate::message::Envelope;

use super::super::{RpcError, limits::runtime};

type ReplySender = oneshot::Sender<Result<Envelope, RpcError>>;

/// 一条客户端连接上尚未收到响应的请求集合。
///
/// `Some(map)` 表示连接仍接受请求；`None` 是不可逆的关闭状态。登记和关闭共用一把锁，
/// 因此最终 drain 完成后不会再插入漏掉的请求。
#[derive(Clone)]
pub(super) struct PendingRequests {
    inner: Arc<Mutex<Option<HashMap<u64, ReplySender>>>>,
}

impl Default for PendingRequests {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Some(HashMap::new()))),
        }
    }
}

impl PendingRequests {
    /// 登记请求并返回响应接收端和取消保护。
    ///
    /// 保护值被丢弃时会移除登记项，所以取消正在排队或等待响应的 future 不会泄漏容量。
    pub(super) fn register(
        &self,
        id: u64,
    ) -> Result<(oneshot::Receiver<Result<Envelope, RpcError>>, PendingCall), RpcError> {
        let (sender, receiver) = oneshot::channel();
        let mut state = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let requests = state.as_mut().ok_or(RpcError::Disconnected)?;
        if requests.len() >= runtime::MAX_PENDING_REQUESTS {
            return Err(RpcError::Overloaded);
        }
        requests.insert(id, sender);
        Ok((
            receiver,
            PendingCall {
                pending: self.clone(),
                id,
            },
        ))
    }

    /// 将响应交给对应请求；迟到或已经取消的响应被安全丢弃。
    pub(super) fn complete(&self, envelope: Envelope) {
        let sender = self
            .inner
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_mut()
            .and_then(|requests| requests.remove(&envelope.request_id));
        if let Some(sender) = sender {
            let _ = sender.send(Ok(envelope));
        }
    }

    /// 进入不可逆关闭状态，并以断开错误完成全部等待者。
    pub(super) fn close(&self) {
        if let Some(requests) = self.inner.lock().unwrap_or_else(|p| p.into_inner()).take() {
            for (_, sender) in requests {
                let _ = sender.send(Err(RpcError::Disconnected));
            }
        }
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .map_or(0, HashMap::len)
    }

    fn cancel(&self, id: u64) {
        if let Some(requests) = self
            .inner
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_mut()
        {
            requests.remove(&id);
        }
    }
}

/// 请求 future 的取消保护；正常完成时对应项已由 [`PendingRequests::complete`] 移除。
pub(super) struct PendingCall {
    pending: PendingRequests,
    id: u64,
}

impl Drop for PendingCall {
    fn drop(&mut self) {
        self.pending.cancel(self.id);
    }
}
