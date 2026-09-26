//! 运行渲染器 RPC 服务，并在连接、UI 线程与配置代理之间路由消息。
//!
//! 每个客户端拥有独立的事件队列和所有者编号；快照通过 UI 命令通道交给界面，
//! UI 事件只返回给仍拥有当前呈现状态的连接。传输层负责有界读写，服务退出时
//! 会终止连接任务并关闭 UI。
use crate::{
    state::Owner,
    theme_api::UiMode,
    ui_runtime::{UiCommand, UiCommandSender, UiHandle},
};
use std::{collections::HashMap, time::Duration};
use tokio::{
    runtime::Builder,
    sync::{mpsc, watch},
    task::JoinSet,
};
use weasel_common::{
    message::{
        Envelope, PeerRole, QueryConfig, RendererEvent, ShutdownResponse, envelope::Payload,
    },
    rpc::{RpcClient, RpcServer, default_renderer_pipe_name, try_default_broker_pipe_name},
};

/// 判断 broker 返回的主题名称是否受当前 renderer 支持。
fn supports_theme(name: &str) -> bool {
    ["eleven", "ten", "abc", "void", "wasm"].contains(&name)
}

/// 启动渲染器运行时、读取主题配置并运行 UI 与 RPC 服务。
///
/// 配置代理不可用或读取失败时记录诊断并采用内嵌配置；运行时或 UI 初始化失败
/// 则返回错误。
pub fn run() -> Result<(), String> {
    let runtime = Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .map_err(|error| format!("could not create renderer runtime: {error}"))?;
    // The live strip uses the broker's published configuration (no disk re-read).
    let settings = runtime.block_on(load_theme(false)).unwrap_or_else(|error| {
        crate::diagnostics::record(format_args!("{error}; using embedded settings"));
        weasel_common::settings::ConfigSnapshot::new(
            serde_json::from_str(include_str!("../../weasel.json"))
                .expect("embedded settings must be valid JSON"),
        )
    });
    let theme = settings.required::<String>(".theme").map_err(|error| {
        let error = format!("invalid renderer theme setting: {error}");
        crate::notifications::invalid_configuration(&error);
        error
    })?;
    let ui = UiHandle::start(&theme, UiMode::Live, &settings)?;
    runtime.block_on(run_rpc(
        RpcServer::with_role(default_renderer_pipe_name(), PeerRole::Renderer),
        ui,
    ))
}

/// 读取 broker 提供的主题和配置。
///
/// `refresh` 会要求 broker 在本次查询中重新读取磁盘配置，使预览能够反映 broker
/// 启动后用户所做的修改。
///
/// 查询和连接均受两秒超时约束；响应必须包含有效配置根，并且主题须受当前后端
/// 支持。此函数不在渲染器进程内直接重读配置文件。
pub(crate) async fn load_theme(
    refresh: bool,
) -> Result<weasel_common::settings::ConfigSnapshot, String> {
    let settings = tokio::time::timeout(Duration::from_secs(2), async {
        let pipe = try_default_broker_pipe_name().map_err(|error| error.to_string())?;
        let client =
            RpcClient::connect_as_with_timeout(pipe, PeerRole::Renderer, Duration::from_secs(2))
                .await
                .map_err(|error| error.to_string())?;
        let response = client
            .request(Payload::QueryConfig(QueryConfig {
                refresh,
                path: ".".into(),
            }))
            .await
            .map_err(|error| error.to_string())?;
        let Some(Payload::ConfigValue(value)) = response.payload else {
            return Err("broker returned an unexpected configuration response".into());
        };
        let json = value.json.ok_or("broker returned no configuration root")?;
        weasel_common::settings::ConfigSnapshot::from_json(&json)
            .map_err(|error| format!("broker returned invalid configuration JSON: {error}"))
    })
    .await
    .map_err(|_| "configuration query timed out".to_owned())??;
    let theme = settings.required::<String>(".theme").map_err(|error| {
        let error = format!("broker returned an invalid renderer theme: {error}");
        crate::notifications::invalid_configuration(&error);
        error
    })?;
    if !supports_theme(&theme) {
        let error = format!("broker returned unsupported renderer theme {theme:?}");
        crate::notifications::invalid_configuration(&error);
        return Err(error);
    }
    Ok(settings)
}

/// 协调 RPC 连接、UI 事件转发、父进程退出和有序关闭。
///
/// 最多同时接纳 64 条客户端连接。每条连接结束时由守卫向 UI 投递断开命令；服务
/// 退出时先取消并回收连接任务，再关闭 UI，并将服务错误与关闭错误合并返回。
async fn run_rpc(server: RpcServer, mut ui: UiHandle) -> Result<(), String> {
    let commands = ui.command_sender();
    let preedit = ui.capabilities.preedit;
    let (shutdown_sender, mut shutdown_receiver) = watch::channel(false);
    let (parent_exit, mut parent_dead) = tokio::sync::oneshot::channel();
    let _parent_watch = weasel_common::service_owner::ParentWatch::start(move || {
        let _ = parent_exit.send(());
    })
    .map_err(|e| e.to_string())?;
    let mut tasks = JoinSet::new();
    let mut routes: HashMap<Owner, mpsc::Sender<RendererEvent>> = HashMap::new();
    let mut next_owner = 0_u64;
    let result = loop {
        tokio::select! {
            _ = &mut parent_dead, if _parent_watch.is_some() => break Ok(()),
            accepted = server.accept(), if routes.len() < 64 => {
                let connection = match accepted {
                    Ok(connection) => connection,
                    Err(error) => break Err(format!("renderer pipe accept failed: {error}")),
                };
                next_owner += 1;
                let owner = next_owner;
                let commands = commands.clone();
                let (events, receiver) = mpsc::channel(32);
                routes.insert(owner, events);
                let shutdown = shutdown_sender.clone();
                tasks.spawn(async move {
                    let _guard = ConnectionOwner { owner, commands: commands.clone() };
                    (owner, serve_connection(connection, owner, commands, receiver, shutdown, preedit).await)
                });
            }
            completed = tasks.join_next(), if !tasks.is_empty() => {
                match completed {
                    Some(Ok((owner, result))) => {
                        routes.remove(&owner);
                        if let Err(error) = result { crate::diagnostics::record(format_args!("client pipe failed: {error}")); }
                    }
                    Some(Err(error)) => break Err(format!("renderer connection task failed: {error}")),
                    None => {}
                }
            }
            event = ui.events.recv() => {
                if let Some((owner, event)) = event {
                    if commands.is_owner(owner) {
                        if let Some(route) = routes.get(&owner) { let _ = route.try_send(event); }
                    }
                } else { break Err("UI event channel closed".into()); }
            }
            finished = &mut ui.finished => {
                break match finished {
                    Ok(Err(error)) => Err(error),
                    Ok(Ok(())) => Err("UI thread exited unexpectedly".into()),
                    Err(_) => Err("UI thread panicked".into()),
                };
            }
            _ = shutdown_receiver.changed() => break Ok(()),
        }
    };
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    routes.clear();
    let close = ui.close();
    result.and(close)
}

/// 连接任务的生命周期守卫，确保连接结束时通知 UI 清理该所有者。
struct ConnectionOwner {
    owner: Owner,
    commands: UiCommandSender,
}

/// 连接所有者离开作用域时请求 UI 处理断开状态；发送失败不阻碍任务退出。
impl Drop for ConnectionOwner {
    fn drop(&mut self) {
        let _ = self.commands.send(UiCommand::Disconnect(self.owner));
    }
}

/// 持续处理一个客户端的请求，并在同一连接上发送发往客户端的 UI 事件。
///
/// 事件写入使用传输层的有界队列，不等待写入确认，以免阻塞快照和断开请求的
/// 接收。只有仍是当前所有者的连接会收到 UI 事件；关闭请求最多等待两秒发送
/// 确认，其他传输或 UI 命令错误会结束该连接并向调用方报告。
async fn serve_connection(
    connection: weasel_common::rpc::RpcConnection,
    owner: Owner,
    commands: UiCommandSender,
    mut events: mpsc::Receiver<RendererEvent>,
    shutdown_sender: watch::Sender<bool>,
    preedit: bool,
) -> Result<(), String> {
    loop {
        // The common transport owns the bounded reader/writer tasks.
        let receive = connection.recv();
        tokio::pin!(receive);
        let envelope = loop {
            tokio::select! {
                result = &mut receive => break result.map_err(|e| e.to_string())?,
                event = events.recv() => {
                    let Some(event) = event else { return Ok(()); };
                    if commands.is_owner(owner) {
                        // The bounded common writer closes the connection on write
                        // failure. Do not stall snapshot/disconnect reads on its ack.
                        let _ack = connection.enqueue(Envelope {
                            request_id: 0,
                            payload: Some(Payload::RendererEvent(event)),
                        }).map_err(|e| e.to_string())?;
                    }
                }
            }
        };
        let Some(envelope) = envelope else {
            return Ok(());
        };
        match envelope.payload {
            Some(Payload::IdentifyService(_)) => {
                connection
                    .enqueue(Envelope {
                        request_id: envelope.request_id,
                        payload: Some(Payload::ServiceIdentity(
                            weasel_common::service_owner::identity("renderer", true),
                        )),
                    })
                    .map_err(|e| e.to_string())?;
            }
            Some(Payload::QueryConfig(query)) => {
                // Read-only runtime capability of the successfully created theme.
                let json = (query.path == ".capabilities.preedit").then(|| preedit.to_string());
                connection
                    .enqueue(Envelope {
                        request_id: envelope.request_id,
                        payload: Some(Payload::ConfigValue(weasel_common::message::ConfigValue {
                            json,
                        })),
                    })
                    .map_err(|e| e.to_string())?;
            }
            Some(Payload::Ping(_)) => {
                connection
                    .send(&Envelope {
                        request_id: envelope.request_id,
                        payload: Some(Payload::Pong(weasel_common::message::Pong {
                            text: "renderer ready".into(),
                        })),
                    })
                    .await
                    .map_err(|e| e.to_string())?;
            }
            Some(Payload::RenderSnapshot(snapshot)) => {
                commands.send(UiCommand::Render(owner, snapshot))?
            }
            Some(Payload::Shutdown(_)) => {
                let response = tokio::time::timeout(
                    Duration::from_secs(2),
                    connection.send(&Envelope {
                        request_id: envelope.request_id,
                        payload: Some(Payload::ShutdownResponse(ShutdownResponse {
                            accepted: true,
                            message: "renderer shutting down".into(),
                        })),
                    }),
                )
                .await;
                let _ = shutdown_sender.send(true);
                response
                    .map_err(|_| "shutdown response timed out")?
                    .map_err(|e| e.to_string())?;
                return Ok(());
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn renderer_role_answers_readiness_without_ui() {
        tokio::time::timeout(Duration::from_secs(5), async {
            let pipe = format!(
                r"\\.\pipe\weasel-renderer-readiness-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            );
            let server = RpcServer::with_role(&pipe, PeerRole::Renderer);
            let accept = tokio::spawn(async move { (server.accept().await.unwrap(), server) });
            let client = loop {
                match weasel_common::rpc::RpcClient::connect_as(&pipe, PeerRole::Server).await {
                    Ok(client) => break client,
                    Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
                }
            };
            let (connection, _server) = accept.await.unwrap();
            let commands = UiCommandSender::without_ui();
            let observer = commands.clone();
            let (_events, receiver) = mpsc::channel(1);
            let (shutdown, _) = watch::channel(false);
            let serve = tokio::spawn(serve_connection(
                connection, 1, commands, receiver, shutdown, true,
            ));
            let response = client
                .request(Payload::Ping(weasel_common::message::Ping {
                    text: "readiness".into(),
                }))
                .await
                .unwrap();
            assert!(matches!(
                response.payload,
                Some(Payload::Pong(weasel_common::message::Pong { ref text }))
                    if text == "renderer ready"
            ));
            assert!(!observer.is_owner(1));
            let response = client
                .request(Payload::QueryConfig(QueryConfig {
                    refresh: false,
                    path: ".capabilities.preedit".into(),
                }))
                .await
                .unwrap();
            assert!(matches!(
                response.payload,
                Some(Payload::ConfigValue(weasel_common::message::ConfigValue {
                    json: Some(ref value),
                })) if value == "true"
            ));
            client.disconnect().await;
            serve.await.unwrap().unwrap();
        })
        .await
        .unwrap();
    }
}
