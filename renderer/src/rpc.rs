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
    message::{Envelope, PeerRole, RendererEvent, ShutdownResponse, envelope::Payload},
    rpc::{RpcServer, default_renderer_pipe_name},
};

pub fn run() -> Result<(), String> {
    let runtime = Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .map_err(|error| format!("could not create renderer runtime: {error}"))?;
    // The live strip uses the broker's published configuration (no disk re-read).
    let settings = runtime.block_on(load_theme(false)).unwrap_or_else(|error| {
        crate::diagnostics::record(format_args!("{error}; using eleven"));
        weasel_common::settings::ConfigSnapshot::new(serde_json::json!({
            "theme": "eleven", "inline_preedit": true, "themeSettings": {}
        }))
    });
    let ui = UiHandle::start(&settings.theme()?, UiMode::Live, &settings)?;
    runtime.block_on(run_rpc(
        RpcServer::with_role(default_renderer_pipe_name(), PeerRole::Renderer),
        ui,
    ))
}

/// Read the broker's theme and its settings. `refresh` asks the broker to re-read
/// the on-disk configuration for this one call, so a preview reflects a setup the
/// user edited after the broker started.
pub(crate) async fn load_theme(
    refresh: bool,
) -> Result<weasel_common::settings::ConfigSnapshot, String> {
    let settings = weasel_common::settings::fetch(PeerRole::Renderer, refresh).await?;
    if !crate::backend::supports_theme(&settings.theme()?) {
        return Err("broker returned an unsupported renderer theme".into());
    }
    Ok(settings)
}

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

struct ConnectionOwner {
    owner: Owner,
    commands: UiCommandSender,
}
impl Drop for ConnectionOwner {
    fn drop(&mut self) {
        let _ = self.commands.send(UiCommand::Disconnect(self.owner));
    }
}

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
                    .send_pong(envelope.request_id, "renderer ready")
                    .await
                    .map_err(|e| e.to_string())?;
            }
            Some(Payload::RenderSnapshot(snapshot)) => {
                commands.send(UiCommand::Render(owner, snapshot))?
            }
            Some(Payload::Shutdown(_)) => {
                let response = tokio::time::timeout(
                    Duration::from_secs(2),
                    connection.send_shutdown_response(
                        envelope.request_id,
                        ShutdownResponse {
                            accepted: true,
                            message: "renderer shutting down".into(),
                        },
                    ),
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
            assert_eq!(
                client.ping("readiness").await.unwrap().text,
                "renderer ready"
            );
            assert!(!observer.is_owner(1));
            assert_eq!(
                client
                    .query_config(".capabilities.preedit", false)
                    .await
                    .unwrap(),
                Some(serde_json::json!(true))
            );
            client.disconnect().await;
            serve.await.unwrap().unwrap();
        })
        .await
        .unwrap();
    }
}
