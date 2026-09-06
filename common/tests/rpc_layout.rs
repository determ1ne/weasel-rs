use std::time::Duration;
use weasel_common::{
    message::{ContextToken, LayoutUpdate, RenderRect, envelope::Payload},
    rpc::{RpcClient, RpcServer},
};

#[tokio::test]
async fn layout_flood_bypasses_input_fifo_and_preserves_final_and_future_geometry() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let pipe = format!(r"\\.\pipe\weasel-layout-test-{}", std::process::id());
        let server = RpcServer::new(&pipe);
        server.bind().await.unwrap();
        let client = RpcClient::connect(&pipe).await.unwrap();
        let connection = server.accept().await.unwrap();
        let token = ContextToken {
            context_id: 1,
            connection_epoch: 1,
            generation: 2,
        };
        let old = ContextToken {
            generation: 1,
            ..token
        };
        for x in 0..10000 {
            client
                .send_layout_update(LayoutUpdate {
                    token: Some(token),
                    session_id: 1,
                    anchor: Some(RenderRect {
                        left: x,
                        ..Default::default()
                    }),
                })
                .await
                .unwrap();
        }
        let ping = tokio::spawn({
            let client = client.clone();
            async move { client.ping("input FIFO still available").await }
        });
        let request = connection.recv().await.unwrap().unwrap();
        assert!(matches!(request.payload, Some(Payload::Ping(_))));
        connection
            .send_pong(request.request_id, "ok")
            .await
            .unwrap();
        ping.await.unwrap().unwrap();
        // The future layout must not be lost while its context command is queued.
        assert!(connection.take_layout_for(Some(&old)).is_none());
        loop {
            if let Some(update) = connection.take_layout_for(Some(&token)) {
                assert_eq!(update.anchor.unwrap().left, 9999);
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(client.is_connected());
        assert!(
            tokio::time::timeout(Duration::from_millis(30), connection.recv())
                .await
                .is_err()
        );
    })
    .await
    .unwrap();
}
