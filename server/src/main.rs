//! 提供进程外运行的 Rime 服务，并独立处理服务控制请求。
//!
//! 本模块负责进程启动、配置读取、连接接纳与优雅关闭；输入任务交由引擎工作线程，
//! 渲染状态则通过 [`renderer_bridge`] 转发给渲染器。
#![windows_subsystem = "windows"]
mod admission;
mod bindings;
mod client_connection;
mod data_lock;
mod deploy_drag;
mod deploy_job;
mod deploy_telemetry;
mod deploy_ui;
mod engine;
mod librime;
mod renderer_bridge;
mod session_route;
mod silent_deploy;
mod ui_bindings;
mod worker;

use client_connection::ClientConnection;
use engine::Work;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use weasel_common::{
    logging::ComponentLogger,
    message::{
        Envelope, Failure, FailureCode, PeerRole, Pong, QueryConfig, ShutdownResponse,
        UserNotification, UserNotificationSeverity, envelope::Payload,
    },
    process::{RuntimePaths, SingleInstance},
    rpc::{RpcClient, RpcServer, default_pipe_name, try_default_broker_pipe_name},
};
/// 可同时接纳的活动客户端上限；等待接纳的连接另受 `admission::PENDING_LIMIT` 限制。
const MAX_CONNECTIONS: usize = 128;

/// 向 broker 查询服务配置。
///
/// 整个连接与请求流程最多等待两秒；连接失败、响应类型不符、配置缺失或 JSON 无效时
/// 返回可用于诊断的错误，由调用方决定是否退回 Rime 默认配置。
async fn load_broker_settings() -> Result<weasel_common::settings::ConfigSnapshot, String> {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        let pipe = try_default_broker_pipe_name().map_err(|error| error.to_string())?;
        let client = RpcClient::connect_as_with_timeout(
            pipe,
            PeerRole::Server,
            std::time::Duration::from_secs(2),
        )
        .await
        .map_err(|error| error.to_string())?;
        let response = client
            .request(Payload::QueryConfig(QueryConfig {
                refresh: false,
                path: ".".into(),
            }))
            .await
            .map_err(|error| error.to_string())?;
        let Some(Payload::ConfigValue(value)) = response.payload else {
            return Err("broker returned an unexpected configuration response".into());
        };
        let json = value.json.ok_or("broker returned no configuration root")?;
        let mut value: serde_json::Value = serde_json::from_str(&json)
            .map_err(|error| format!("broker returned invalid configuration JSON: {error}"))?;
        if !value.is_object() {
            return Err("broker returned a non-object configuration root".into());
        }
        let warnings = normalize_server_settings(&mut value);
        if !warnings.is_empty() {
            let notice = UserNotification {
                source: "server".into(),
                code: "configuration.invalid".into(),
                severity: UserNotificationSeverity::Warning as i32,
                title: "小狼毫RS：配置项无效".into(),
                message: "算法服务配置无效，已使用内置默认值。".into(),
                details: warnings.join("\n"),
            };
            if let Err(error) = client.request(Payload::UserNotification(notice)).await {
                eprintln!("weasel-server: could not report invalid settings: {error}");
            }
        }
        Ok(weasel_common::settings::ConfigSnapshot::new(value))
    })
    .await
    .map_err(|_| "configuration query timed out".to_owned())?
}

/// 校验 server 自己消费的字段，并将无效值逐项恢复为打包默认值。
///
/// 未知字段保持原样，以便 renderer、主题或未来组件自行解释。应用专属选项中的
/// 无效字段会被移除，从而重新继承对应全局值。
fn normalize_server_settings(root: &mut serde_json::Value) -> Vec<String> {
    let defaults: serde_json::Value = serde_json::from_str(include_str!("../../weasel.json"))
        .expect("embedded settings must be valid JSON");
    let defaults = defaults
        .as_object()
        .expect("embedded settings root must be an object");
    let root = root
        .as_object_mut()
        .expect("configuration root was checked before normalization");
    let mut warnings = Vec::new();
    let checks = [
        (
            "theme",
            root.get("theme").is_some_and(serde_json::Value::is_string),
        ),
        (
            "inline_preedit",
            root.get("inline_preedit")
                .is_some_and(serde_json::Value::is_boolean),
        ),
        (
            "global_ascii_status",
            root.get("global_ascii_status")
                .is_some_and(serde_json::Value::is_boolean),
        ),
        (
            "ascii_mode",
            root.get("ascii_mode")
                .is_some_and(serde_json::Value::is_boolean),
        ),
        (
            "allow_rime_in_secure_fields",
            root.get("allow_rime_in_secure_fields")
                .is_some_and(serde_json::Value::is_boolean),
        ),
    ];
    for (name, valid) in checks {
        if !valid {
            root.insert(
                name.into(),
                defaults
                    .get(name)
                    .expect("embedded server setting must exist")
                    .clone(),
            );
            warnings.push(format!("{name} has an invalid type"));
        }
    }

    if !root
        .get("app_options")
        .is_some_and(serde_json::Value::is_object)
    {
        root.insert(
            "app_options".into(),
            defaults
                .get("app_options")
                .expect("embedded app_options must exist")
                .clone(),
        );
        warnings.push("app_options must be an object".into());
        return warnings;
    }

    let default_apps = defaults
        .get("app_options")
        .and_then(serde_json::Value::as_object)
        .expect("embedded app_options must be an object");
    let apps = root
        .get_mut("app_options")
        .and_then(serde_json::Value::as_object_mut)
        .expect("app_options was checked above");
    for name in apps.keys().cloned().collect::<Vec<_>>() {
        let valid_name = !name.is_empty() && !name.contains(['/', '\\']);
        let valid_object = apps.get(&name).is_some_and(serde_json::Value::is_object);
        if !valid_name || !valid_object {
            if let Some(default) = default_apps.get(&name) {
                apps.insert(name.clone(), default.clone());
            } else {
                apps.remove(&name);
            }
            warnings.push(format!(
                "app_options.{name} must be an executable basename object"
            ));
            continue;
        }
        let options = apps
            .get_mut(&name)
            .and_then(serde_json::Value::as_object_mut)
            .expect("application options were checked above");
        let default_options = default_apps
            .get(&name)
            .and_then(serde_json::Value::as_object);
        for option in ["ascii_mode", "inline_preedit"] {
            if options.get(option).is_some_and(|value| !value.is_boolean()) {
                if let Some(default) = default_options.and_then(|values| values.get(option)) {
                    options.insert(option.into(), default.clone());
                } else {
                    options.remove(option);
                }
                warnings.push(format!("app_options.{name}.{option} must be a boolean"));
            }
        }
    }
    warnings
}

/// 标记客户端处理任务仍存活，并在任务退出时唤醒引擎处理队列。
///
/// 存活标志由任务与其排入引擎的工作共享；释放守卫时先发布退出状态，再调用通知器。
struct ClientLife(Arc<AtomicBool>, Arc<dyn Fn() + Send + Sync>);
impl Drop for ClientLife {
    /// 发布客户端退出状态，并唤醒可能正在等待该状态的引擎线程。
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
        (self.1)();
    }
}
/// 服务端收到关闭请求后交给主循环的排空信息。
struct ShutdownNotice {
    /// 保持连接存活，直到关闭响应有机会写出。
    connection: Arc<ClientConnection>,
    /// 响应写入结果；入队失败时为空，主循环最多等待有限时间。
    written: Option<tokio::sync::oneshot::Receiver<Result<(), weasel_common::rpc::RpcError>>>,
}

/// 直接生成无需引擎或会话参与的控制面响应。
///
/// 未就绪时拒绝 Ping；关闭请求一经接受即返回响应并交由主循环执行关闭，其他载荷
/// 返回 `None`，继续由连接处理逻辑分派。
fn control_reply(envelope: &Envelope, ready: bool) -> Option<Envelope> {
    let payload = match envelope.payload.as_ref()? {
        Payload::IdentifyService(_) => {
            Payload::ServiceIdentity(weasel_common::service_owner::identity("server", ready))
        }
        Payload::Ping(_) if !ready => Payload::Failure(Failure {
            code: FailureCode::NotReady as i32,
            message: "engine not ready".into(),
        }),
        Payload::Ping(ping) => Payload::Pong(Pong {
            text: ping.text.clone(),
        }),
        Payload::Shutdown(_) => Payload::ShutdownResponse(ShutdownResponse {
            accepted: true,
            message: "server shutdown accepted".into(),
        }),
        _ => return None,
    };
    Some(Envelope {
        request_id: envelope.request_id,
        payload: Some(payload),
    })
}

/// 持续读取一个客户端连接，并将输入消息非阻塞地投递到引擎队列。
///
/// 控制请求在本任务内应答，布局更新只更新连接缓存。处于待接纳状态的连接必须先通过
/// 首个输入上下文的令牌校验才能占用活动名额；等待期间仍受接纳截止时间约束。工作项
/// 持有请求完成守卫和客户端存活标志，使断连能够被引擎观察到。
async fn read_connection(
    client_id: u64,
    connection: Arc<ClientConnection>,
    engine: worker::Sender<Work>,
    ready: Arc<AtomicBool>,
    shutdown: tokio::sync::mpsc::Sender<ShutdownNotice>,
    mut admission: Option<admission::Admission>,
) {
    let life = ClientLife(Arc::new(AtomicBool::new(true)), engine.notifier());
    connection.set_notifier(
        admission
            .as_ref()
            .map_or_else(|| engine.notifier(), |a| a.notifier(engine.notifier())),
    );
    let mut pending_opened = None;
    let mut pending_mode_restored = false;
    loop {
        let received = if let Some(a) = admission.as_ref().filter(|a| a.pending()) {
            match tokio::time::timeout_at(a.deadline, connection.recv_tracked()).await {
                Ok(result) => result,
                Err(_) => break,
            }
        } else {
            connection.recv_tracked().await
        };
        let (envelope, request) = match received {
            Ok(Some(received)) => received,
            _ => break,
        };
        if let Some(Payload::LayoutUpdate(update)) = envelope.payload.as_ref() {
            connection.push_layout(update.clone());
            drop(request);
            continue;
        }
        if let Some(reply) = control_reply(&envelope, ready.load(Ordering::Acquire)) {
            let written = connection.enqueue(reply);
            if matches!(envelope.payload, Some(Payload::Shutdown(_))) {
                let _ = shutdown.try_send(ShutdownNotice {
                    connection: connection.clone(),
                    written: written.ok(),
                });
                break;
            }
            if written.is_err() {
                break;
            }
            continue;
        }
        if let Some(a) = admission.as_mut().filter(|a| a.pending()) {
            use weasel_common::message::ContextAction;
            match envelope.payload.as_ref() {
                Some(Payload::OpenInput(open)) if pending_opened.is_none() => {
                    pending_opened = open.token
                }
                Some(Payload::LogEvent(_)) => {}
                // Restore remembered mode before Focus without claiming an active
                // slot. Allow only one assignment for this opened context; the
                // pending connection's original deadline still applies.
                Some(Payload::ContextCommand(command))
                    if command.action == ContextAction::SetAscii as i32
                        && command.ascii_mode.is_some()
                        && !pending_mode_restored
                        && pending_opened.is_some()
                        && command.token == pending_opened =>
                {
                    pending_mode_restored = true;
                }
                Some(Payload::KeyEvent(_) | Payload::ContextCommand(_)) => {
                    let token = match envelope.payload.as_ref() {
                        Some(Payload::KeyEvent(key)) => key.token,
                        Some(Payload::ContextCommand(command))
                            if command.action == ContextAction::Focus as i32 =>
                        {
                            command.token
                        }
                        _ => None,
                    };
                    let valid = pending_opened.zip(token).is_some_and(|(opened, token)| {
                        token.context_id == opened.context_id
                            && token.connection_epoch == opened.connection_epoch
                            && token.generation >= opened.generation
                    });
                    if !valid || !a.promote(client_id, &connection).await {
                        break;
                    }
                }
                _ => break,
            }
        }
        match envelope.payload.as_ref() {
            Some(Payload::OpenInput(_) | Payload::KeyEvent(_) | Payload::ContextCommand(_)) => {
                if engine
                    .try_send(Work::Message {
                        client_id,
                        connection: connection.clone(),
                        alive: life.0.clone(),
                        envelope,
                        _request: request,
                    })
                    .is_err()
                {
                    tracing::warn!(
                        client_id,
                        "engine queue unavailable; closing input connection"
                    );
                    break;
                }
            }
            Some(Payload::LogEvent(_)) => {
                // Client diagnostics can contain input: never persist their payload.
                tracing::debug!(client_id, "client diagnostic received");
            }
            _ => (),
        }
    }
}

/// 解析服务启动参数并映射启动结果为进程退出码。
///
/// 静默部署模式不向继承句柄输出参数错误；普通模式将启动失败写入标准错误。
fn main() -> std::process::ExitCode {
    let args = match weasel_common::service_owner::take_launch_args(std::env::args_os().skip(1)) {
        Ok(args) => args,
        Err(error) => {
            eprintln!("Error: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    if args.iter().any(|arg| arg == "--silent") {
        if !silent_deploy::valid_arguments(&args) {
            // Silent mode must not leak even argument errors to inherited handles.
            return std::process::ExitCode::FAILURE;
        }
        return silent_deploy::run();
    }
    match run(args) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Error: {error:?}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[tokio::main]
/// 根据命令行模式启动部署界面、独立部署任务或常驻服务。
///
/// 常驻服务使用单实例锁；部署在具名线程中持有用户数据锁，线程错误和 panic
/// 都会转换为启动错误。
async fn run(args: Vec<std::ffi::OsString>) -> Result<(), Box<dyn std::error::Error>> {
    let deploy_ui_mode = args.iter().any(|arg| arg == "--deploy-ui");
    let deploy_mode = args.iter().any(|arg| arg == "--deploy");
    let paths = RuntimePaths::discover()?;
    paths.ensure()?;
    if deploy_ui_mode {
        init_logging(&paths, &format!("deploy-ui-{}", std::process::id()))?;
        return deploy_ui::run().map_err(Into::into);
    }
    let _instance = match SingleInstance::acquire("server") {
        Ok(instance) => instance,
        Err(error) => {
            if deploy_mode {
                return Err(error.into());
            }
            tracing::info!("server instance already running");
            return Ok(());
        }
    };
    if deploy_mode {
        return std::thread::Builder::new()
            .name("rime-deploy".into())
            .spawn(move || {
                let _data_lock = data_lock::DataLock::acquire(&paths.user_data)?;
                init_logging(&paths, "deploy")?;
                librime::Librime::deploy(&paths.executable_directory, &paths.user_data)
            })?
            .join()
            .map_err(|_| "deployment thread panicked")?
            .map_err(Into::into);
    }
    serve(paths).await.map_err(Into::into)
}

/// 按运行时路径为指定组件安装日志订阅器。
///
/// 调用失败时返回初始化错误；`try_init` 不会替换进程中已安装的全局订阅器。
fn init_logging(paths: &RuntimePaths, component: &str) -> Result<(), String> {
    let logger = ComponentLogger::for_paths(paths, component).map_err(|error| error.to_string())?;
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_timer(weasel_common::logging::UtcTimer)
        .with_writer(logger)
        .try_init()
        .map_err(|error| error.to_string())
}

/// 运行服务主循环，并在任一关闭条件触发后有序停止所有后台任务。
///
/// 监听器错误会在引擎完成关闭后返回；broker 失联、收到关闭请求或引擎结束则进入
/// 清理流程。关闭响应最多排空 250 毫秒，连接读取任务与渲染桥接任务都会被终止并汇合。
async fn serve(paths: RuntimePaths) -> Result<(), String> {
    let (parent_exit, mut parent_dead) = tokio::sync::oneshot::channel();
    let _parent_watch = weasel_common::service_owner::ParentWatch::start(move || {
        let _ = parent_exit.send(());
    })
    .map_err(|e| e.to_string())?;
    let settings = match load_broker_settings().await {
        Ok(settings) => Some(settings),
        Err(error) => {
            eprintln!("weasel-server: broker settings unavailable: {error}; using Rime defaults");
            None
        }
    };
    let (publisher, snapshots) = renderer_bridge::RendererPublisher::channel();
    let capability = publisher.clone();
    let eager_renderer = match settings.as_ref() {
        Some(settings) => {
            let external_preedit = settings.needs_external_preedit().unwrap_or_else(|error| {
                tracing::error!(%error, "invalid preedit setting; using inline preedit fallback");
                false
            });
            let wasm = settings
                .required::<String>(".theme")
                .map(|theme| theme == "wasm")
                .unwrap_or_else(|error| {
                    tracing::error!(%error, "invalid theme setting; using default renderer policy");
                    false
                });
            external_preedit || wasm
        }
        None => false,
    };
    let mut engine = worker::Worker::spawn(engine::QUEUE_CAPACITY, move || {
        engine::Engine::new(paths, publisher, settings)
    })?;
    let renderer =
        renderer_bridge::spawn(snapshots, engine.sender.clone(), capability, eager_renderer);
    let server = RpcServer::with_role(
        default_pipe_name(),
        weasel_common::message::PeerRole::Server,
    );
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::channel(1);

    let mut readers = tokio::task::JoinSet::new();
    let mut connections = std::collections::HashMap::<u64, Arc<ClientConnection>>::new();
    let gate = admission::Gate::new(MAX_CONNECTIONS);
    let mut task_clients = std::collections::HashMap::new();
    let mut next_client_id = 1_u64;
    let mut notice = None;
    let mut listener_error = None;
    tracing::info!("server control listener started");
    loop {
        tokio::select! {
            biased;
            _ = &mut parent_dead, if _parent_watch.is_some() => { tracing::info!("broker exited; shutting down"); break; }
            request = shutdown_rx.recv() => { notice = request; break; }
            _ = &mut engine.finished => { break; }
            Some(result) = readers.join_next_with_id(), if !readers.is_empty() => {
                let task = match result { Ok((task, _)) => task, Err(error) => error.id() };
                if let Some(id) = task_clients.remove(&task) {
                    connections.remove(&id);
                    gate.peers.lock().unwrap_or_else(|p| p.into_inner()).remove(&id);
                }
            },
            accepted = server.accept() => {
                let connection = match accepted {
                    Ok(connection) => Arc::new(ClientConnection::new(connection)),
                    Err(error) => { listener_error = Some(error.to_string()); break; }
                };
                if connections.len() >= MAX_CONNECTIONS + admission::PENDING_LIMIT { continue; }
                let client_id = next_client_id;
                next_client_id = next_client_id.wrapping_add(1).max(1);
                let Some(admission) = gate.start(client_id) else { continue; };
                connections.insert(client_id, connection.clone());
                gate.peers.lock().unwrap_or_else(|p| p.into_inner()).insert(client_id, connection.clone());
                let sender = engine.sender.clone();
                let ready = engine.ready.clone();
                let shutdown = shutdown_tx.clone();
                let task = readers.spawn(async move {
                    read_connection(client_id, connection, sender, ready, shutdown, Some(admission)).await;
                    client_id
                });
                task_clients.insert(task.id(), client_id);
            }
        }
    }
    engine.request_stop();
    drop(server);
    readers.abort_all();
    while readers.join_next().await.is_some() {}
    renderer.abort();
    let _ = renderer.await;
    let drain_ack = async {
        if let Some(notice) = notice {
            if let Some(written) = notice.written {
                let _ = tokio::time::timeout(std::time::Duration::from_millis(250), written).await;
            }
            drop(notice.connection);
        }
    };
    let (result, ()) = tokio::join!(engine.shutdown(), drain_ack);
    result?;
    tracing::info!("engine finalized and joined; server stopped gracefully");
    if let Some(error) = listener_error {
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod control_tests {
    use super::*;
    use weasel_common::message::{Ping, Shutdown};

    #[tokio::test]
    async fn ping_and_shutdown_bypass_blocked_engine_and_full_mailbox() {
        use std::{sync::mpsc, time::Duration};
        use weasel_common::{message::PeerRole, rpc::RpcClient};
        struct NoInput;
        impl worker::Processor<Work> for NoInput {
            fn process(&mut self, _: Work) {
                panic!("control connection reached engine");
            }
        }
        let (release, blocked) = mpsc::channel();
        let engine = worker::Worker::spawn(1, move || {
            blocked
                .recv_timeout(Duration::from_secs(5))
                .map_err(|error| error.to_string())?;
            Ok(NoInput)
        })
        .unwrap();
        engine
            .sender
            .try_send(Work::Renderer(Default::default()))
            .ok()
            .unwrap();
        assert!(
            engine
                .sender
                .try_send(Work::Renderer(Default::default()))
                .is_err()
        );
        let name = format!(
            r"\\.\pipe\weasel-server-control-test-{}",
            std::process::id()
        );
        let server = RpcServer::new(&name);
        let client = async {
            loop {
                if let Ok(client) = RpcClient::connect_as(&name, PeerRole::Broker).await {
                    break client;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        };
        let (connection, client) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(server.accept(), client)
        })
        .await
        .unwrap();
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::channel(1);
        let reader = tokio::spawn(read_connection(
            1,
            Arc::new(ClientConnection::new(connection.unwrap())),
            engine.sender.clone(),
            engine.ready.clone(),
            shutdown_tx,
            None,
        ));
        assert!(
            tokio::time::timeout(
                Duration::from_secs(1),
                client.request(Payload::Ping(Ping {
                    text: "ready".into(),
                }))
            )
            .await
            .unwrap()
            .is_err()
        );
        let response = tokio::time::timeout(
            Duration::from_secs(1),
            client.request(Payload::Shutdown(Shutdown {
                reason: "test".into(),
            })),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(matches!(
            response.payload,
            Some(Payload::ShutdownResponse(ShutdownResponse {
                accepted: true,
                ..
            }))
        ));
        let notice = shutdown_rx.recv().await.unwrap();
        engine.request_stop();
        release.send(()).unwrap();
        engine.shutdown().await.unwrap();
        reader.await.unwrap();
        drop(notice);
        client.disconnect().await;
    }
    #[test]
    fn control_requests_have_replies_without_an_engine_or_session() {
        for payload in [
            Payload::Ping(Ping {
                text: "ready".into(),
            }),
            Payload::Shutdown(Shutdown {
                reason: "test".into(),
            }),
        ] {
            let reply = control_reply(
                &Envelope {
                    request_id: 17,
                    payload: Some(payload),
                },
                true,
            )
            .unwrap();
            assert_eq!(reply.request_id, 17);
            assert!(matches!(
                reply.payload,
                Some(Payload::Pong(_) | Payload::ShutdownResponse(_))
            ));
        }
        assert!(
            control_reply(
                &Envelope {
                    request_id: 1,
                    payload: Some(Payload::KeyEvent(Default::default()))
                },
                true
            )
            .is_none()
        );
    }
}
