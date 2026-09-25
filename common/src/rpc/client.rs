//! 与业务消息无关的 Named Pipe 双向 RPC 客户端。

use std::sync::Arc;

use tokio::{
    net::windows::named_pipe::ClientOptions,
    sync::{broadcast, mpsc, watch},
};

use crate::message::{Envelope, envelope::Payload};

use super::{
    RpcError, codec,
    limits::{protocol, runtime},
};

mod connection;
mod pending;

use connection::ClientTasks;
use pending::PendingRequests;

#[cfg(test)]
#[path = "tests/client.rs"]
mod tests;

/// 已连接的双向 RPC 客户端。
///
/// 克隆客户端会共享同一条管道及其后台任务；丢弃最后一个克隆时，后台任务随之结束。
/// 请求通过请求 ID 与响应关联，普通请求最多等待五秒；调用被取消时会移除其待处理记录。
/// 连接关闭会使尚未完成的请求以断开错误结束。服务器主动发送的事件则可由各自的订阅接收。
#[derive(Clone)]
pub struct RpcClient {
    server_pid: u32,
    outbound: mpsc::Sender<Envelope>,
    pending: PendingRequests,
    closed: watch::Sender<bool>,
    incoming: broadcast::Sender<Envelope>,
    next_request_id: Arc<std::sync::atomic::AtomicU64>,
    tasks: Arc<ClientTasks>,
}

impl RpcClient {
    /// 返回操作系统报告的管道服务端进程 ID。
    ///
    /// 该值来自已连接管道的内核信息，而非服务端在协议中自报的进程 ID。
    pub fn server_pid(&self) -> u32 {
        self.server_pid
    }
    /// 连接指定的 Named Pipe，并启动后台读写任务。
    ///
    /// 以未指定的本端角色完成握手；需要声明角色时使用 [`Self::connect_as`]。
    pub async fn connect(pipe_name: impl AsRef<str>) -> Result<Self, RpcError> {
        Self::connect_as(pipe_name, crate::message::PeerRole::Unspecified).await
    }

    /// 在总超时期限内连接，并仅对管道实例暂时繁忙的错误重试。
    ///
    /// 超时只覆盖连接尝试；后续 RPC 请求应由调用方纳入自己的操作期限。
    pub async fn connect_as_with_timeout(
        pipe_name: impl AsRef<str>,
        role: crate::message::PeerRole,
        timeout: std::time::Duration,
    ) -> Result<Self, RpcError> {
        tokio::time::timeout(timeout, async {
            loop {
                match Self::connect_as(pipe_name.as_ref(), role).await {
                    Err(RpcError::Io(error))
                        if error.raw_os_error()
                            == Some(crate::bindings::ERROR_PIPE_BUSY as i32) =>
                    {
                        tokio::time::sleep(runtime::CONNECT_RETRY_DELAY).await;
                    }
                    result => return result,
                }
            }
        })
        .await
        .map_err(|_| RpcError::Timeout)?
    }

    /// 以指定的本端协议角色连接 Named Pipe，并启动后台读写任务。
    pub async fn connect_as(
        pipe_name: impl AsRef<str>,
        role: crate::message::PeerRole,
    ) -> Result<Self, RpcError> {
        let pipe = ClientOptions::new()
            .read(true)
            .write(true)
            .open(pipe_name.as_ref())?;
        use std::os::windows::io::AsRawHandle;
        let mut server_pid = 0;
        unsafe {
            let _ = crate::bindings::GetNamedPipeServerProcessId(
                crate::bindings::HANDLE(pipe.as_raw_handle()),
                &mut server_pid,
            );
        }
        let pending = PendingRequests::default();
        let connection = connection::spawn(pipe, role, pending.clone());

        Ok(Self {
            server_pid,
            outbound: connection.outbound,
            pending,
            closed: connection.closed,
            incoming: connection.incoming,
            next_request_id: Arc::new(std::sync::atomic::AtomicU64::new(
                protocol::FIRST_REQUEST_ID,
            )),
            tasks: connection.tasks,
        })
    }

    /// 等待连接关闭；若连接已经关闭则立即返回。
    pub async fn disconnected(&self) {
        let mut closed = self.closed.subscribe();
        let _ = closed.wait_for(|closed| *closed).await;
    }

    /// 检查客户端是否仍将连接视为可用。
    pub fn is_connected(&self) -> bool {
        !*self.closed.borrow()
    }

    /// 订阅对端发来的完整消息流。
    ///
    /// 请求响应和主动事件均按管道接收顺序发布；调用方负责按业务载荷过滤。响应会先发布到
    /// 此流，再唤醒对应的 [`Self::request`]，因此需要统一顺序的消费者不会发生重排。
    pub fn subscribe(&self) -> broadcast::Receiver<Envelope> {
        self.incoming.subscribe()
    }

    /// 关闭连接、取消未完成请求并终止后台读写任务。
    ///
    /// 对所有共享此连接的客户端克隆生效；重复调用是安全的。
    pub async fn disconnect(&self) {
        self.pending.close();
        self.closed.send_replace(true);
        self.tasks.abort_all().await;
    }

    /// 发布无需响应的事件。
    ///
    /// 载荷以请求 ID 0 排入有界 FIFO。队列已满时关闭连接，避免调用方继续在已丢失顺序的
    /// 通道上发送业务消息。
    pub fn publish(&self, payload: Payload) -> Result<(), RpcError> {
        if !self.is_connected() {
            return Err(RpcError::Disconnected);
        }
        self.outbound
            .try_send(Envelope {
                request_id: protocol::EVENT_REQUEST_ID,
                payload: Some(payload),
            })
            .map_err(|error| self.queue_error(error))
    }

    fn queue_error(&self, error: mpsc::error::TrySendError<Envelope>) -> RpcError {
        match error {
            mpsc::error::TrySendError::Full(_) => {
                self.closed.send_replace(true);
                RpcError::Overloaded
            }
            mpsc::error::TrySendError::Closed(_) => RpcError::Disconnected,
        }
    }

    /// 发送请求并等待具有相同请求编号的响应。
    ///
    /// 此层只负责编号、关联、期限和远端失败；响应载荷的业务类型由调用组件解释。
    pub async fn request(&self, payload: Payload) -> Result<Envelope, RpcError> {
        if !self.is_connected() {
            return Err(RpcError::Disconnected);
        }
        // Validate before queueing: a caller error must not poison a live connection.
        codec::validate_request(&payload)?;
        let id = self
            .next_request_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if id == protocol::EVENT_REQUEST_ID {
            self.disconnect().await;
            return Err(RpcError::Protocol("request ID exhausted".into()));
        }
        let (rx, _call) = self.pending.register(id)?;
        // Cancelling this future drops the registration, even during queueing.
        tokio::time::timeout(runtime::REQUEST_TIMEOUT, async {
            self.outbound
                .send(Envelope {
                    request_id: id,
                    payload: Some(payload),
                })
                .await
                .map_err(|_| RpcError::Disconnected)?;
            let reply = rx.await.map_err(|_| RpcError::Disconnected)??;
            if let Some(Payload::Failure(error)) = &reply.payload {
                return Err(RpcError::Remote {
                    code: crate::message::FailureCode::try_from(error.code)
                        .map_err(|_| RpcError::Protocol("unknown failure code".into()))?,
                    message: error.message.clone(),
                });
            }
            Ok(reply)
        })
        .await
        .map_err(|_| RpcError::Timeout)?
    }
}
