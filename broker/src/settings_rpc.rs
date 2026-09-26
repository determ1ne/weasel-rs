//! 提供仅限当前登录会话使用的配置查询与用户通知 RPC 端点。
//!
//! 配置查询读取已发布的有效快照；显式刷新则临时从磁盘重载并返回结果，
//! 不会替换该快照。磁盘操作被移出异步运行时，并发读取受信号量限制。
use std::{
    sync::{Arc, RwLock},
    thread,
    time::Duration,
};
use tokio::{
    runtime::Builder,
    sync::{Semaphore, oneshot},
    task::JoinSet,
};
use weasel_common::{
    message::{Envelope, PeerRole, envelope::Payload},
    process::RuntimePaths,
    rpc::{RpcConnection, RpcError, RpcServer, try_default_broker_pipe_name},
    settings::ConfigSnapshot as Settings,
};

/// 持有配置 RPC 监听器及其工作线程的服务句柄。
///
/// 丢弃句柄会请求服务停止，并在有限时间内等待线程清理。
pub struct SettingsService {
    /// 供本地调用方使用的通知中心句柄。
    notifications: crate::notifications::NotificationCenter,
    /// 可被服务端发布、并由查询端读取的有效配置快照。
    settings: SettingsStore,
    /// 向服务线程发送停止信号；取出后表示停止请求已发出。
    stop: Option<oneshot::Sender<()>>,
    /// 服务线程完成清理后发送的通知，用于限制析构时的等待时间。
    done: std::sync::mpsc::Receiver<()>,
    /// 持有 RPC 运行时的服务线程句柄。
    worker: Option<thread::JoinHandle<()>>,
}

/// 在线程间共享的有效配置快照。
///
/// 替换会先完成写锁下的发布；之后读取快照的调用方（包括新连接）都能观察到新值。
#[derive(Clone)]
pub struct SettingsStore(Arc<RwLock<Settings>>);
impl SettingsStore {
    /// 原子地发布新的有效配置；若锁曾发生中毒，则恢复其内部值后继续。
    pub fn replace(&self, settings: Settings) {
        *self.0.write().unwrap_or_else(|p| p.into_inner()) = settings;
    }

    /// 克隆当前完整快照，使调用方在释放读锁后仍可独立查询。
    fn snapshot(&self) -> Settings {
        self.0.read().unwrap_or_else(|p| p.into_inner()).clone()
    }
}

impl SettingsService {
    /// 获取通知中心的共享句柄。
    pub fn notifications(&self) -> crate::notifications::NotificationCenter {
        self.notifications.clone()
    }

    /// 获取有效配置快照的共享发布句柄。
    pub fn settings(&self) -> SettingsStore {
        self.settings.clone()
    }

    /// 在当前进程的默认 broker 管道上启动配置与通知服务。
    ///
    /// 绑定监听器成功后才返回；管道名、运行时或绑定错误会作为错误返回。
    pub fn start(
        settings: Settings,
        paths: RuntimePaths,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        Self::start_on(try_default_broker_pipe_name()?, settings, paths)
    }

    /// 在指定管道启动服务。监听器就绪后才启动工作线程，确保调用方可安全启动子进程。
    fn start_on(
        pipe: String,
        settings: Settings,
        paths: RuntimePaths,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let runtime = Builder::new_current_thread().enable_all().build()?;
        let server = RpcServer::with_role(pipe, PeerRole::Broker);
        // Publish the listener before the broker starts renderer.
        runtime.block_on(server.bind())?;
        let (stop, mut stopping) = oneshot::channel();
        let (finished, done) = std::sync::mpsc::channel();
        let notifications = crate::notifications::NotificationCenter::new(&paths);
        let notification_handle = notifications.clone();
        let settings = SettingsStore(Arc::new(RwLock::new(settings)));
        let published = settings.clone();
        let disk_reads = Arc::new(Semaphore::new(1));
        let worker = thread::Builder::new().name("broker-settings".into()).spawn(move || {
            runtime.block_on(async move {
                let mut clients = JoinSet::new();
                loop {
                    tokio::select! {
                        _ = &mut stopping => break,
                        accepted = server.accept(), if clients.len() < 32 => {
                            match accepted {
                                Ok(connection) => {
                                    let settings = settings.clone();
                                    let notifications = notifications.clone();
                                    let paths = paths.clone();
                                    let disk_reads = disk_reads.clone();
                                    clients.spawn(async move {
                                        // Bound idle/misbehaving clients as well as the connection count.
                                        let _ = tokio::time::timeout(Duration::from_secs(5), serve(connection, settings, paths, disk_reads, notifications)).await;
                                    });
                                }
                                Err(error) => {
                                    eprintln!("weasel-broker: settings listener failed: {error}");
                                    break;
                                }
                            }
                        }
                        _ = clients.join_next(), if !clients.is_empty() => {}
                    }
                }
                clients.abort_all();
                while clients.join_next().await.is_some() {}
            });
            // A blocked filesystem operation cannot be cancelled by aborting its
            // async waiter. Do not let runtime Drop wait indefinitely for it.
            runtime.shutdown_timeout(Duration::from_millis(100));
            let _ = finished.send(());
        })?;
        Ok(Self {
            settings: published,
            notifications: notification_handle,
            stop: Some(stop),
            done,
            worker: Some(worker),
        })
    }
}

impl Drop for SettingsService {
    /// 请求工作线程停止，并最多等待两秒完成清理；超时后分离线程句柄。
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if self.done.recv_timeout(Duration::from_secs(2)).is_ok() {
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }
}

/// 顺序处理一个已接受连接上的通知和配置查询请求。
///
/// 通知写入完成后才回复；刷新查询从磁盘读取并报告配置错误，但不发布快照。连接收发
/// 失败会向调用方返回 RPC 错误，连接级超时由服务端任务负责施加。
async fn serve(
    connection: RpcConnection,
    settings: SettingsStore,
    paths: RuntimePaths,
    disk_reads: Arc<Semaphore>,
    notifications: crate::notifications::NotificationCenter,
) -> Result<(), RpcError> {
    while let Some(request) = connection.recv().await? {
        if let Some(Payload::UserNotification(notice)) = &request.payload {
            let center = notifications.clone();
            let notice = notice.clone();
            read_on_worker(disk_reads.clone(), move || center.report(notice)).await?;
            connection
                .send(&Envelope {
                    request_id: request.request_id,
                    payload: Some(Payload::Pong(weasel_common::message::Pong {
                        text: "notification recorded".into(),
                    })),
                })
                .await?;
            continue;
        }
        if let Some(Payload::QueryConfig(query)) = &request.payload {
            // A refresh re-reads the on-disk configuration for this one call so a
            // just-edited setup is previewed; it does not publish to the store.
            let payload = if query.refresh {
                let paths = paths.clone();
                let path = query.path.clone();
                let notifications = notifications.clone();
                read_on_worker(disk_reads.clone(), move || {
                    let mut warnings = Vec::new();
                    let fresh = crate::settings::load(&paths, |warning| warnings.push(warning));
                    if warnings.is_empty() {
                        config_payload(&fresh, &path)
                    } else {
                        notifications.report_settings_errors(&warnings);
                        // A preview must not claim rejected disk settings were applied.
                        Payload::Failure(weasel_common::message::Failure {
                            code: weasel_common::message::FailureCode::InvalidArgument as i32,
                            message: warnings.join("; "),
                        })
                    }
                })
                .await?
            } else {
                config_payload(&settings.snapshot(), &query.path)
            };
            connection
                .send(&Envelope {
                    request_id: request.request_id,
                    payload: Some(payload),
                })
                .await?;
        }
    }
    Ok(())
}

/// 将配置路径查询结果编码为 RPC 负载；无匹配值表示为 `json: None`，无效路径返回失败负载。
fn config_payload(snapshot: &Settings, path: &str) -> Payload {
    match snapshot.query(path) {
        Ok(value) => Payload::ConfigValue(weasel_common::message::ConfigValue {
            json: value.map(ToString::to_string),
        }),
        Err(message) => Payload::Failure(weasel_common::message::Failure {
            code: weasel_common::message::FailureCode::InvalidArgument as i32,
            message,
        }),
    }
}

/// 在阻塞线程池执行磁盘工作，并以信号量限制同时进行的读取数。
///
/// 许可由阻塞闭包持有，因此取消等待它的 RPC 不会提前释放名额；关闭信号量或任务失败
/// 分别映射为断连错误和协议错误。
async fn read_on_worker<T: Send + 'static>(
    gate: Arc<Semaphore>,
    operation: impl FnOnce() -> T + Send + 'static,
) -> Result<T, RpcError> {
    let permit = gate
        .acquire_owned()
        .await
        .map_err(|_| RpcError::Disconnected)?;
    tokio::task::spawn_blocking(move || {
        // Own the permit inside the blocking operation, even if its RPC is cancelled.
        let _permit = permit;
        operation()
    })
    .await
    .map_err(|_| RpcError::Protocol("configuration reader failed".into()))
}

#[cfg(test)]
#[path = "../tests/unit/settings_rpc.rs"]
mod tests;
