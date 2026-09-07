//! Read-only configuration endpoint, confined to the current logon session.
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
    rpc::{RpcConnection, RpcError, RpcServer, try_default_broker_pipe_name},
    runtime_paths::RuntimePaths,
    settings::ConfigSnapshot as Settings,
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
    pub fn start(
        settings: Settings,
        paths: RuntimePaths,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        Self::start_on(try_default_broker_pipe_name()?, settings, paths)
    }

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
                                    let paths = paths.clone();
                                    let disk_reads = disk_reads.clone();
                                    clients.spawn(async move {
                                        // Bound idle/misbehaving clients as well as the connection count.
                                        let _ = tokio::time::timeout(Duration::from_secs(5), serve(connection, settings, paths, disk_reads)).await;
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

async fn serve(
    connection: RpcConnection,
    settings: SettingsStore,
    paths: RuntimePaths,
    disk_reads: Arc<Semaphore>,
) -> Result<(), RpcError> {
    while let Some(request) = connection.recv().await? {
        if let Some(Payload::QueryConfig(query)) = &request.payload {
            // A refresh re-reads the on-disk configuration for this one call so a
            // just-edited setup is previewed; it does not publish to the store.
            let payload = if query.refresh {
                let paths = paths.clone();
                let path = query.path.clone();
                read_on_worker(disk_reads.clone(), move || {
                    let mut warnings = Vec::new();
                    let fresh = crate::settings::load(&paths, |warning| warnings.push(warning));
                    if warnings.is_empty() {
                        config_payload(&fresh, &path)
                    } else {
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
mod tests {
    use super::*;
    use weasel_common::rpc::RpcClient;

    async fn connect(pipe: &str, role: PeerRole) -> Result<RpcClient, RpcError> {
        RpcClient::connect_as_with_timeout(pipe, role, Duration::from_secs(1)).await
    }

    #[test]
    fn slow_disk_does_not_block_runtime_and_cancel_keeps_permit() {
        let runtime = Builder::new_current_thread().enable_all().build().unwrap();
        let gate = Arc::new(Semaphore::new(1));
        let (release, wait) = std::sync::mpsc::channel();
        runtime.block_on(async {
            let (started, ready) = oneshot::channel();
            let task = tokio::spawn(read_on_worker(gate.clone(), move || {
                let _ = started.send(());
                let _ = wait.recv_timeout(Duration::from_secs(2));
            }));
            tokio::time::timeout(Duration::from_secs(1), ready)
                .await
                .unwrap()
                .unwrap();
            // Runtime remains responsive while the filesystem worker is waiting.
            tokio::time::sleep(Duration::from_millis(5)).await;
            assert_eq!(gate.available_permits(), 0);
            task.abort();
            let _ = task.await;
            assert_eq!(gate.available_permits(), 0);
            release.send(()).unwrap();
            let permit = tokio::time::timeout(Duration::from_secs(1), gate.acquire())
                .await
                .unwrap()
                .unwrap();
            drop(permit);
        });
    }

    static TEST_DIR: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    fn test_paths() -> RuntimePaths {
        let id = TEST_DIR.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let base = format!("weasel-settings-test-{}-{}", std::process::id(), id);
        let root = std::env::temp_dir().join(&base);
        RuntimePaths {
            executable_directory: root.clone(),
            development: true,
            user_data: root.join("user-data"),
            logs: root.join("logs"),
        }
    }

    #[test]
    fn concurrent_queries_and_role_restrictions() {
        let pipe = weasel_common::platform::RuntimeIdentity::current()
            .unwrap()
            .pipe_name(&format!("settings-test-{}", std::process::id()))
            .unwrap();
        let paths = test_paths();
        let service = SettingsService::start_on(
            pipe.clone(),
            Settings::new(serde_json::json!({"theme":"ten"})),
            paths.clone(),
        )
        .unwrap();
        let runtime = Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(5), async {
                let a = connect(&pipe, PeerRole::Renderer).await.unwrap();
                let b = connect(&pipe, PeerRole::Renderer).await.unwrap();
                let (a_result, b_result) =
                    tokio::join!(a.query_config(".", false), b.query_config(".", false));
                assert_eq!(a_result.unwrap().unwrap()["theme"], "ten");
                assert_eq!(b_result.unwrap().unwrap()["theme"], "ten");
                let engine = connect(&pipe, PeerRole::Server).await.unwrap();
                assert_eq!(
                    engine.query_config(".theme", false).await.unwrap(),
                    Some(serde_json::json!("ten"))
                );
                assert!(
                    engine
                        .query_config(".missing", false)
                        .await
                        .unwrap()
                        .is_none()
                );
                assert!(
                    engine
                        .query_config(".theme | invalid", false)
                        .await
                        .is_err()
                );
                service
                    .settings()
                    .replace(Settings::new(serde_json::json!({"theme":"eleven"})));
                // Existing and newly accepted clients observe the published snapshot.
                assert_eq!(
                    b.query_config(".", false).await.unwrap().unwrap()["theme"],
                    "eleven"
                );
                let c = connect(&pipe, PeerRole::Renderer).await.unwrap();
                assert_eq!(
                    c.query_config(".", false).await.unwrap().unwrap()["theme"],
                    "eleven"
                );
                // The read-only endpoint does not accept lifecycle commands.
                assert!(a.shutdown("not allowed").await.is_err());
                let tip = connect(&pipe, PeerRole::Tip).await.unwrap();
                assert!(tip.query_config(".", false).await.is_err());
                assert_eq!(
                    b.query_config(".", false).await.unwrap().unwrap()["theme"],
                    "eleven"
                );
            })
            .await
            .unwrap();
        });
        drop(service);
        // Drop closes the listener and releases first-instance ownership.
        let _replacement = SettingsService::start_on(
            pipe,
            Settings::new(serde_json::json!({"theme":"eleven"})),
            paths,
        )
        .unwrap();
    }

    #[test]
    fn refresh_reloads_from_disk_without_publishing() {
        let pipe = weasel_common::platform::RuntimeIdentity::current()
            .unwrap()
            .pipe_name(&format!("settings-refresh-test-{}", std::process::id()))
            .unwrap();
        let paths = test_paths();
        let custom = paths.user_data.join("weasel.custom.json");
        let _ = std::fs::remove_file(&custom);
        std::fs::create_dir_all(&paths.user_data).unwrap();
        let service = SettingsService::start_on(
            pipe.clone(),
            Settings::new(serde_json::json!({"theme":"ten"})),
            paths.clone(),
        )
        .unwrap();
        let runtime = Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(5), async {
                let client = connect(&pipe, PeerRole::Renderer).await.unwrap();
                // Baseline: a non-refresh query returns the published snapshot.
                assert_eq!(
                    client.query_config(".", false).await.unwrap().unwrap()["theme"],
                    "ten"
                );
                // The user edits the configuration on disk.
                std::fs::write(&custom, br#"{"theme":"eleven","preview":true}"#).unwrap();
                // A refresh re-reads the fresh disk state and returns the complete merged object.
                let fresh = client.query_config(".", true).await.unwrap().unwrap();
                assert_eq!(fresh["theme"], "eleven");
                assert_eq!(fresh["preview"], true);
                // The refresh does not publish: the published snapshot is unchanged.
                assert_eq!(
                    client.query_config(".", false).await.unwrap().unwrap()["theme"],
                    "ten"
                );
                std::fs::write(
                    &custom,
                    br#"{"theme":"ten","themeSettings":{"ten":{"color":"red"}}}"#,
                )
                .unwrap();
                let fresh = client.query_config(".", true).await.unwrap().unwrap();
                let json = fresh;
                assert_eq!(json["themeSettings"]["ten"]["color"], "red");
                assert_eq!(
                    client.query_config(".", false).await.unwrap().unwrap()["theme"],
                    "ten"
                );
                std::fs::write(&custom, b"{invalid").unwrap();
                assert!(matches!(
                    client.query_config(".", true).await,
                    Err(RpcError::Remote { .. })
                ));
                assert_eq!(
                    client.query_config(".", false).await.unwrap().unwrap()["theme"],
                    "ten"
                );
            })
            .await
            .unwrap();
        });
        drop(service);
        let _ = std::fs::remove_file(&custom);
        let _ = std::fs::remove_dir(&paths.user_data);
        let _ = std::fs::remove_dir(&paths.executable_directory);
    }
}
