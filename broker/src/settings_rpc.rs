//! Read-only configuration endpoint, confined to the current logon session.
use std::{
    sync::{Arc, RwLock},
    thread,
    time::Duration,
};
use tokio::{runtime::Builder, sync::oneshot, task::JoinSet};
use weasel_common::{
    message::{Envelope, PeerRole, Settings, envelope::Payload},
    rpc::{RpcConnection, RpcError, RpcServer, try_default_broker_pipe_name},
};

pub struct SettingsService {
    settings: SettingsStore,
    stop: Option<oneshot::Sender<()>>,
    done: std::sync::mpsc::Receiver<()>,
    worker: Option<thread::JoinHandle<()>>,
}

/// Shared effective snapshot. Publication completes before replacement children start.
#[derive(Clone)]
pub struct SettingsStore(Arc<RwLock<Settings>>);
impl SettingsStore {
    pub fn replace(&self, settings: Settings) {
        *self.0.write().unwrap_or_else(|p| p.into_inner()) = settings;
    }
    fn snapshot(&self) -> Settings {
        self.0.read().unwrap_or_else(|p| p.into_inner()).clone()
    }
}

impl SettingsService {
    pub fn settings(&self) -> SettingsStore {
        self.settings.clone()
    }
    pub fn start(settings: Settings) -> Result<Self, Box<dyn std::error::Error>> {
        Self::start_on(try_default_broker_pipe_name()?, settings)
    }

    fn start_on(pipe: String, settings: Settings) -> Result<Self, Box<dyn std::error::Error>> {
        let runtime = Builder::new_current_thread().enable_all().build()?;
        let server = RpcServer::with_role(pipe, PeerRole::Broker);
        // Publish the listener before the broker starts renderer.
        runtime.block_on(server.bind())?;
        let (stop, mut stopping) = oneshot::channel();
        let (finished, done) = std::sync::mpsc::channel();
        let settings = SettingsStore(Arc::new(RwLock::new(settings)));
        let published = settings.clone();
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
                                    clients.spawn(async move {
                                        // Bound idle/misbehaving clients as well as the connection count.
                                        let _ = tokio::time::timeout(Duration::from_secs(5), serve(connection, settings)).await;
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
            drop(runtime);
            let _ = finished.send(());
        })?;
        Ok(Self {
            settings: published,
            stop: Some(stop),
            done,
            worker: Some(worker),
        })
    }
}

impl Drop for SettingsService {
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

async fn serve(connection: RpcConnection, settings: SettingsStore) -> Result<(), RpcError> {
    while let Some(request) = connection.recv().await? {
        if matches!(request.payload, Some(Payload::GetSettings(_))) {
            connection
                .send(&Envelope {
                    request_id: request.request_id,
                    payload: Some(Payload::Settings(settings.snapshot())),
                })
                .await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use weasel_common::rpc::RpcClient;

    #[test]
    fn concurrent_queries_and_role_restrictions() {
        let pipe = weasel_common::platform::RuntimeIdentity::current()
            .unwrap()
            .pipe_name(&format!("settings-test-{}", std::process::id()))
            .unwrap();
        let service = SettingsService::start_on(
            pipe.clone(),
            Settings {
                theme: "ten".into(),
            },
        )
        .unwrap();
        let runtime = Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(5), async {
                let a = RpcClient::connect_as(&pipe, PeerRole::Renderer)
                    .await
                    .unwrap();
                let b = RpcClient::connect_as(&pipe, PeerRole::Renderer)
                    .await
                    .unwrap();
                let (a_result, b_result) = tokio::join!(a.get_settings(), b.get_settings());
                assert_eq!(a_result.unwrap().theme, "ten");
                assert_eq!(b_result.unwrap().theme, "ten");
                service.settings().replace(Settings {
                    theme: "eleven".into(),
                });
                // Existing and newly accepted clients observe the published snapshot.
                assert_eq!(b.get_settings().await.unwrap().theme, "eleven");
                let c = RpcClient::connect_as(&pipe, PeerRole::Renderer)
                    .await
                    .unwrap();
                assert_eq!(c.get_settings().await.unwrap().theme, "eleven");
                // The read-only endpoint does not accept lifecycle commands.
                assert!(a.shutdown("not allowed").await.is_err());
                let tip = RpcClient::connect_as(&pipe, PeerRole::Tip).await.unwrap();
                assert!(tip.get_settings().await.is_err());
                assert_eq!(b.get_settings().await.unwrap().theme, "eleven");
            })
            .await
            .unwrap();
        });
        drop(service);
        // Drop closes the listener and releases first-instance ownership.
        let _replacement = SettingsService::start_on(
            pipe,
            Settings {
                theme: "eleven".into(),
            },
        )
        .unwrap();
    }
}
