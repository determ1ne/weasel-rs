use super::*;
use weasel_common::{
    message::{QueryConfig, Shutdown, UserNotification, envelope::Payload},
    rpc::RpcClient,
};

struct TestClient(RpcClient);

impl TestClient {
    async fn query_config(
        &self,
        path: &str,
        refresh: bool,
    ) -> Result<Option<serde_json::Value>, RpcError> {
        match self
            .0
            .request(Payload::QueryConfig(QueryConfig {
                refresh,
                path: path.into(),
            }))
            .await?
            .payload
        {
            Some(Payload::ConfigValue(value)) => value
                .json
                .map(|json| {
                    serde_json::from_str(&json)
                        .map_err(|error| RpcError::Protocol(error.to_string()))
                })
                .transpose(),
            _ => Err(RpcError::UnexpectedResponse),
        }
    }

    async fn notify_user(&self, notice: UserNotification) -> Result<(), RpcError> {
        match self
            .0
            .request(Payload::UserNotification(notice))
            .await?
            .payload
        {
            Some(Payload::Pong(_)) => Ok(()),
            _ => Err(RpcError::UnexpectedResponse),
        }
    }

    async fn shutdown(&self, reason: &str) -> Result<(), RpcError> {
        match self
            .0
            .request(Payload::Shutdown(Shutdown {
                reason: reason.into(),
            }))
            .await?
            .payload
        {
            Some(Payload::ShutdownResponse(_)) => Ok(()),
            _ => Err(RpcError::UnexpectedResponse),
        }
    }
}

async fn connect(pipe: &str, role: PeerRole) -> Result<TestClient, RpcError> {
    RpcClient::connect_as_with_timeout(pipe, role, Duration::from_secs(1))
        .await
        .map(TestClient)
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
        user_data: root.join("user-data"),
        logs: root.join("logs"),
    }
}

#[test]
fn concurrent_queries_and_role_restrictions() {
    let pipe = weasel_common::windows_security::RuntimeIdentity::current()
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
            let notice = weasel_common::message::UserNotification {
                source: "server".into(),
                code: "configuration.invalid".into(),
                severity: weasel_common::message::UserNotificationSeverity::Warning as i32,
                title: "Configuration warning".into(),
                message: "Using defaults".into(),
                details: "field=example".into(),
            };
            engine.notify_user(notice.clone()).await.unwrap();
            a.notify_user(notice).await.unwrap();
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
    let pipe = weasel_common::windows_security::RuntimeIdentity::current()
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
