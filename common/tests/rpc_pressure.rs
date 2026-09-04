use std::time::Duration;
use weasel_common::{
    message::{Envelope, LogEvent, envelope::Payload},
    rpc::{RpcClient, RpcError, RpcServer},
};

#[tokio::test]
async fn full_writer_queue_fails_one_peer_without_blocking_control_connection() {
    tokio::time::timeout(Duration::from_secs(4), async {
        let name = format!(r"\\.\pipe\weasel-pressure-{}", std::process::id());
        let server = RpcServer::new(&name);
        assert!(
            tokio::time::timeout(Duration::from_millis(1), server.accept())
                .await
                .is_err()
        );
        let slow = RpcClient::connect(&name).await.unwrap();
        let connection = server.accept().await.unwrap();
        let mut overloaded = false;
        // No yielding: model the producer outpacing a blocked pipe writer.
        for _ in 0..256 {
            match connection.enqueue(Envelope {
                request_id: 0,
                payload: Some(Payload::LogEvent(LogEvent {
                    level: "info".into(),
                    text: "test".into(),
                })),
            }) {
                Err(RpcError::Overloaded) => {
                    overloaded = true;
                    break;
                }
                Ok(_) => (),
                Err(error) => panic!("unexpected error: {error}"),
            }
        }
        assert!(overloaded);
        let control = RpcClient::connect(&name).await.unwrap();
        let connection = server.accept().await.unwrap();
        let service = tokio::spawn(async move {
            let request = connection.recv().await.unwrap().unwrap();
            assert!(matches!(request.payload, Some(Payload::Ping(_))));
            connection
                .send_pong(request.request_id, "responsive")
                .await
                .unwrap();
            connection
        });
        assert_eq!(control.ping("control").await.unwrap().text, "responsive");
        drop(service.await.unwrap());
        drop(slow);
    })
    .await
    .expect("a saturated peer must not stall a second connection");
}
