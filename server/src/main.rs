//! Out-of-process Rime service with an independent control plane.
#![cfg_attr(windows, windows_subsystem = "windows")]
mod admission;
mod bindings;
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

use engine::Work;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use weasel_common::{
    logging::ComponentLogger,
    message::{Envelope, Failure, FailureCode, Pong, ShutdownResponse, envelope::Payload},
    process::SingleInstance,
    rpc::{RpcConnection, RpcServer, default_pipe_name},
    runtime_paths::RuntimePaths,
};
const MAX_CONNECTIONS: usize = 128;

struct ClientLife(Arc<AtomicBool>, Arc<dyn Fn() + Send + Sync>);
impl Drop for ClientLife {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
        (self.1)();
    }
}
struct ShutdownNotice {
    connection: Arc<RpcConnection>,
    written: Option<tokio::sync::oneshot::Receiver<Result<(), weasel_common::rpc::RpcError>>>,
}

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

async fn read_connection(
    client_id: u64,
    connection: Arc<RpcConnection>,
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
                Some(Payload::KeyEvent(_) | Payload::ContextCommand(_)) => {
                    let token = match envelope.payload.as_ref() {
                        Some(Payload::KeyEvent(key)) if !key.test && key.keycode.is_some() => {
                            key.token
                        }
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

fn main() -> std::process::ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.iter().any(|arg| arg == "--silent") {
        if !silent_deploy::valid_arguments(&args) {
            // Silent mode must not leak even argument errors to inherited handles.
            return std::process::ExitCode::FAILURE;
        }
        return silent_deploy::run();
    }
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Error: {error:?}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[tokio::main]
async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let deploy_ui_mode = std::env::args().any(|arg| arg == "--deploy-ui");
    let deploy_mode = std::env::args().any(|arg| arg == "--deploy");
    let paths = RuntimePaths::discover()?;
    weasel_common::input_diagnostics::enable(paths.development);
    paths.ensure()?;
    if paths.development && !deploy_ui_mode && !deploy_mode {
        unsafe {
            let _ = bindings::AllocConsole();
        }
    }
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
                librime::Librime::deploy(&paths.executable_directory)
            })?
            .join()
            .map_err(|_| "deployment thread panicked")?
            .map_err(Into::into);
    }
    serve(paths).await.map_err(Into::into)
}

fn init_logging(paths: &RuntimePaths, component: &str) -> Result<(), String> {
    use tracing_subscriber::fmt::writer::MakeWriterExt;
    let logger = ComponentLogger::for_paths(paths, component).map_err(|error| error.to_string())?;
    if paths.development {
        tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(logger.and(std::io::stderr))
            .try_init()
            .map_err(|error| error.to_string())
    } else {
        tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(logger)
            .try_init()
            .map_err(|error| error.to_string())
    }
}

async fn serve(paths: RuntimePaths) -> Result<(), String> {
    let (parent_exit, mut parent_dead) = tokio::sync::oneshot::channel();
    let _parent_watch = weasel_common::service_owner::ParentWatch::start(move || {
        let _ = parent_exit.send(());
    })
    .map_err(|e| e.to_string())?;
    let settings =
        match weasel_common::settings::fetch(weasel_common::message::PeerRole::Server, false).await
        {
            Ok(settings) => Some(settings),
            Err(error) => {
                eprintln!(
                    "weasel-server: broker settings unavailable: {error}; using Rime defaults"
                );
                None
            }
        };
    let (publisher, snapshots) = renderer_bridge::RendererPublisher::channel();
    let capability = publisher.clone();
    let eager_renderer = settings
        .as_ref()
        .is_some_and(|s| s.needs_external_preedit());
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
    let mut connections = std::collections::HashMap::<u64, Arc<RpcConnection>>::new();
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
                    Ok(connection) => Arc::new(connection),
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
            Arc::new(connection.unwrap()),
            engine.sender.clone(),
            engine.ready.clone(),
            shutdown_tx,
            None,
        ));
        assert!(
            tokio::time::timeout(Duration::from_secs(1), client.ping("ready"))
                .await
                .unwrap()
                .is_err()
        );
        assert!(
            tokio::time::timeout(Duration::from_secs(1), client.shutdown("test"))
                .await
                .unwrap()
                .unwrap()
                .accepted
        );
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
