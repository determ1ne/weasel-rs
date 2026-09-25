//! Windows 命名管道 RPC 服务端原语。
//!
//! [`RpcServer`] 持有监听端点和预创建的下一个管道实例；[`RpcConnection`] 表示已接入的对端，
//! 由独立读写任务处理数据帧。对端角色及允许的消息由协议字段约束，但角色字段由对端自报，
//! 不构成身份认证；实际访问控制由管道安全描述符提供。

use tokio::{
    net::windows::named_pipe::{NamedPipeServer, ServerOptions},
    sync::{Mutex, mpsc, oneshot, watch},
};

use crate::message::{Envelope, PeerRole};

use super::{RpcError, limits::runtime};

mod connection;
mod policy;

use connection::Outgoing;

/// 持有 Windows 命名管道监听端点，并缓存一个待连接实例。
///
/// 每个已接入客户端占用一个独立管道实例。`accept` 返回连接前会先创建下一个实例，
/// 以便监听端点持续可用。取消正在等待的 `accept` 不会丢弃缓存实例或其上正在连接的客户端。
///
/// 监听器本身不提供身份认证；使用 [`RpcServer::with_role`] 可严格限制协议角色，
/// 而 [`RpcServer::new`] 为兼容现有调用方还允许未指定角色。
pub struct RpcServer {
    pipe_name: String,
    instance_id: u64,
    role: PeerRole,
    allow_unspecified_peer: bool,
    next_instance: Mutex<Option<NamedPipeServer>>,
}

/// 一个已连接的双向 RPC 通道。
///
/// 连接接管已连接的管道句柄，并启动后台读写任务。接收队列和发送队列各有固定容量；
/// 丢弃此值会关闭通道并中止其后台任务。
pub struct RpcConnection {
    client_executable: Option<String>,
    activity: std::sync::Arc<super::activity::Activity>,
    incoming: Mutex<mpsc::Receiver<Result<Envelope, RpcError>>>,
    outgoing: mpsc::Sender<Outgoing>,
    closed: watch::Sender<bool>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Drop for RpcConnection {
    fn drop(&mut self) {
        self.closed.send_replace(true);
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl RpcServer {
    /// 创建服务端角色的监听器，并允许未指定角色的现有调用方。
    ///
    /// 管道名由调用方提供；实例 ID 在本监听器生命周期内保持不变。若需拒绝未声明角色的
    /// 客户端或使用其他协议角色，请改用 [`Self::with_role`]。
    pub fn new(pipe_name: impl Into<String>) -> Self {
        let mut server = Self::with_role(pipe_name, PeerRole::Server);
        // Compatibility for existing raw/test clients using RpcClient::connect.
        server.allow_unspecified_peer = true;
        server
    }

    /// 创建指定协议角色的严格监听器。
    ///
    /// 角色决定可接受的对端角色和消息种类，但角色由对端在握手中自报，不能用于认证。
    /// 服务端角色使用供沙箱 TIP 使用的输入管道 ACL；其他角色使用当前登录身份的私有管道 ACL。
    /// 此构造器拒绝未指定角色的对端。管道实例仅在监听器内部延迟创建；创建失败会在
    /// [`Self::bind`] 或 [`Self::accept`] 时作为 [`RpcError`] 返回。
    pub fn with_role(pipe_name: impl Into<String>, role: PeerRole) -> Self {
        Self {
            pipe_name: pipe_name.into(),
            role,
            allow_unspecified_peer: false,
            instance_id: (std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64
                ^ u64::from(std::process::id()))
            .max(1),
            next_instance: Mutex::new(None),
        }
    }

    /// 返回监听器使用的管道名。
    ///
    /// 返回值借用监听器，不转移字符串所有权。
    pub fn pipe_name(&self) -> &str {
        &self.pipe_name
    }

    fn create_instance(&self, first: bool) -> Result<NamedPipeServer, RpcError> {
        let identity = crate::windows_security::RuntimeIdentity::current()?;
        let descriptor = if self.role == PeerRole::Server {
            crate::windows_security::LocalSecurityDescriptor::for_input_pipe(&identity)?
        } else {
            crate::windows_security::LocalSecurityDescriptor::for_named_pipe(&identity)?
        };
        let mut attributes = descriptor.security_attributes();
        let mut options = ServerOptions::new();
        options
            .reject_remote_clients(true)
            .first_pipe_instance(first);
        // SAFETY: attributes has the SECURITY_ATTRIBUTES ABI and references the
        // owned descriptor, both alive through this synchronous creation call.
        // Windows copies the descriptor; neither pointer survives the call.
        let pipe = unsafe {
            options.create_with_security_attributes_raw(
                &self.pipe_name,
                (&mut attributes as *mut crate::windows_security::SecurityAttributes).cast(),
            )?
        };
        Ok(pipe)
    }

    /// 等待一个客户端连接并返回其 RPC 通道。
    ///
    /// 等待期间持有内部实例锁，因此同一监听器上的并发调用会串行化。取消等待不会丢弃
    /// 正在连接的实例。连接建立后，会先创建后续实例再返回；任一管道创建、连接或安全描述符
    /// 操作失败均返回 [`RpcError`]。返回的连接拥有已连接管道，并启动收发任务。
    pub async fn accept(&self) -> Result<RpcConnection, RpcError> {
        // Keep the instance in listener state across select! cancellation.
        let mut next_instance = self.next_instance.lock().await;
        if next_instance.is_none() {
            *next_instance = Some(self.create_instance(true)?);
        }
        next_instance.as_ref().unwrap().connect().await?;
        // The connected instance remains alive, so the namespace is continuously
        // held while adding subsequent instances for simultaneous clients.
        let replacement = self.create_instance(false)?;
        let pipe = next_instance.replace(replacement).unwrap();
        drop(next_instance);
        Ok(RpcConnection::from_pipe(
            pipe,
            self.instance_id,
            self.role,
            self.allow_unspecified_peer,
        ))
    }

    /// 预先创建首个管道实例，但不等待客户端连接。
    ///
    /// 可在启动客户端前调用；重复调用在实例已创建时不产生额外实例。管道或安全描述符
    /// 创建失败时返回 [`RpcError`]。
    pub async fn bind(&self) -> Result<(), RpcError> {
        let mut next = self.next_instance.lock().await;
        if next.is_none() {
            *next = Some(self.create_instance(true)?);
        }
        Ok(())
    }
}

impl RpcConnection {
    fn from_pipe(
        pipe: NamedPipeServer,
        instance_id: u64,
        role: PeerRole,
        allow_unspecified_peer: bool,
    ) -> Self {
        let connection = connection::spawn(pipe, instance_id, role, allow_unspecified_peer);
        Self {
            client_executable: connection.client_executable,
            activity: connection.activity,
            incoming: Mutex::new(connection.incoming),
            outgoing: connection.outgoing,
            closed: connection.closed,
            tasks: connection.tasks,
        }
    }

    /// 接收下一条业务消息；连接关闭且队列耗尽时返回 `Ok(None)`。
    ///
    /// 每次调用串行消费同一个接收队列。底层帧、解码或协议校验失败作为 [`RpcError`] 返回；
    /// 该错误同时终止读任务。返回的信封由调用方拥有，但此接口不保留引擎处理期间的
    /// 回收保护；需要该保护时使用 [`Self::recv_tracked`]。
    pub async fn recv(&self) -> Result<Option<Envelope>, RpcError> {
        Ok(self
            .recv_tracked()
            .await?
            .map(|(envelope, _lease)| envelope))
    }

    /// 返回接入时解析出的客户端可执行文件名（不含目录）。
    ///
    /// 进程信息只查询一次；查询权限不足、进程已退出或路径无法解码时返回 `None`。
    /// 返回的字符串借用连接，不转移所有权。
    pub fn client_executable(&self) -> Option<&str> {
        self.client_executable.as_deref()
    }

    /// 接收消息并附带请求租约。
    ///
    /// 收到业务请求后立即建立租约；租约在其值被丢弃时释放，因此应将它持有到引擎完成
    /// 该请求的处理。租约存活期间该连接不会成为空闲回收候选。队列关闭且耗尽时返回
    /// `Ok(None)`，接收或协议错误以 [`RpcError`] 返回。
    pub async fn recv_tracked(&self) -> Result<Option<(Envelope, super::RequestLease)>, RpcError> {
        Ok(self
            .incoming
            .lock()
            .await
            .recv()
            .await
            .transpose()?
            .map(|envelope| (envelope, super::RequestLease(self.activity.clone()))))
    }

    /// 设置连接活动变化时调用的唤醒回调。
    ///
    /// 回调由共享活动状态持有，并在状态锁之外执行；设置后会立即调用一次以触发重新检查。
    /// 后续设置会替换先前回调。回调应快速返回，避免阻塞执行它的任务。
    pub fn set_notifier(&self, notify: std::sync::Arc<dyn Fn() + Send + Sync>) {
        self.activity.set_notifier(notify);
    }
    /// 标记连接是否处于可回收的空闲状态。
    ///
    /// 忙碌状态会阻止基于空闲时长的回收；调用方负责在状态改变时更新标记。
    pub fn set_idle(&self, idle: bool) {
        self.activity.set_idle(idle);
    }
    /// 在当前不存在在途请求或写入时，取得连接退役令牌。
    ///
    /// 令牌只描述传输活动；输入状态、业务优先级与候选排序由调用组件维护。
    pub fn retire_candidate(&self) -> Option<super::RetireCandidate> {
        self.activity.candidate()
    }
    /// 仅当活动状态仍与 `expected` 一致时退役并关闭连接。
    pub fn retire(&self, expected: super::RetireCandidate) -> bool {
        self.activity.retire(expected, || {
            self.closed.send_replace(true);
        })
    }
    /// 等待连接进入关闭状态。
    ///
    /// 连接任务结束、显式回收或丢弃连接都会通知等待者；此方法不消费连接。
    pub async fn disconnected(&self) {
        let mut closed = self.closed.subscribe();
        let _ = closed.wait_for(|closed| *closed).await;
    }
    /// 若连接满足空闲条件且已至少空闲 `age`，返回其最后活动时间。
    ///
    /// 该查询不会关闭连接；实际回收须调用 [`Self::evict_if_stale`]，后者会再次检查条件。
    pub fn stale_since(&self, age: std::time::Duration) -> Option<std::time::Instant> {
        self.activity.stale_since(age)
    }
    /// 若连接已空闲至少 `age` 且没有在途请求或写入，则退役并关闭连接。
    ///
    /// 不满足条件或已退役时返回 `false`；成功关闭时返回 `true`。需要业务优先级的调用方
    /// 应先组合自己的状态与 [`Self::retire_candidate`]，再调用 [`Self::retire`]。
    pub fn evict_if_stale(&self, age: std::time::Duration) -> bool {
        self.activity.evict(age, || {
            self.closed.send_replace(true);
        })
    }

    /// 将信封按入队顺序加入有界发送队列，不等待对端完成读取。
    ///
    /// 信封会被移动到队列中。成功时返回的接收端可等待该帧写入结果；队列已满时返回
    /// [`RpcError::Overloaded`] 并关闭连接，通道已关闭时返回 [`RpcError::Disconnected`]。
    /// 成功入队不代表对端已收到消息；写入错误或超时由返回的应答通道报告。
    pub fn enqueue(
        &self,
        envelope: Envelope,
    ) -> Result<oneshot::Receiver<Result<(), RpcError>>, RpcError> {
        if *self.closed.borrow() {
            return Err(RpcError::Disconnected);
        }
        let (tx, rx) = oneshot::channel();
        if !self.activity.write() {
            return Err(RpcError::Disconnected);
        }
        self.outgoing.try_send((envelope, tx)).map_err(|error| {
            self.activity.finish_write();
            match error {
                mpsc::error::TrySendError::Full(_) => {
                    self.closed.send_replace(true);
                    RpcError::Overloaded
                }
                mpsc::error::TrySendError::Closed(_) => RpcError::Disconnected,
            }
        })?;
        Ok(rx)
    }

    /// 发送信封并等待本地管道写入完成。
    ///
    /// 信封通过克隆后入队，调用方保留原值。写入等待最多三秒；队列过载、连接断开、
    /// 应答通道关闭、写入失败或超时均返回 [`RpcError`]。成功仅表示数据写入管道，
    /// 不表示对端业务处理已经完成。
    pub async fn send(&self, envelope: &Envelope) -> Result<(), RpcError> {
        let reply = self.enqueue(envelope.clone())?;
        tokio::time::timeout(runtime::SEND_ACK_TIMEOUT, reply)
            .await
            .map_err(|_| RpcError::Timeout)?
            .map_err(|_| RpcError::Disconnected)?
    }
}
