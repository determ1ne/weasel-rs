use std::time::Duration;
mod support;
use support::{ClientProtocol as _, ConnectionProtocol as _};
use weasel_common::rpc::{RpcClient, RpcServer};

#[tokio::test]
async fn stale_eviction_waits_for_request_and_reply_then_notifies_disconnect() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let pipe = format!(r"\\.\pipe\weasel-eviction-{}", std::process::id());
        let server = RpcServer::new(&pipe);
        server.bind().await.unwrap();
        let client = RpcClient::connect(&pipe).await.unwrap();
        let connection = server.accept().await.unwrap();
        let ping = tokio::spawn({
            let client = client.clone();
            async move { client.ping("test").await }
        });
        let (request, lease) = connection.recv_tracked().await.unwrap().unwrap();
        connection.set_idle(true);
        assert!(!connection.evict_if_stale(Duration::ZERO));
        connection
            .send_pong(request.request_id, "ok")
            .await
            .unwrap();
        ping.await.unwrap().unwrap();
        drop(lease);
        assert!(connection.evict_if_stale(Duration::ZERO));
        client.disconnected().await;
        assert!(connection.recv().await.unwrap().is_none());
        assert!(client.ping("retired").await.is_err());
    })
    .await
    .unwrap();
}
