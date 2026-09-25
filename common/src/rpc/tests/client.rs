use super::*;
use std::time::Duration;

#[tokio::test]
async fn cancelled_requests_remove_pending_and_last_clone_drop_closes_pipe() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let name = format!(r"\\.\pipe\weasel-cancel-pending-{}", std::process::id());
        let server = super::super::RpcServer::new(&name);
        assert!(
            tokio::time::timeout(Duration::from_millis(1), server.accept())
                .await
                .is_err()
        );
        let client = RpcClient::connect(&name).await.unwrap();
        let connection = server.accept().await.unwrap();
        let drain = tokio::spawn(async move {
            let mut received = 0;
            while connection.recv().await.unwrap().is_some() {
                received += 1;
            }
            received
        });
        // 超过 pending 上限的连续取消不得留下注册项，也不得阻止最后一个客户端关闭管道。
        for _ in 0..80 {
            assert!(
                tokio::time::timeout(
                    Duration::from_millis(1),
                    client.request(crate::message::envelope::Payload::Ping(
                        crate::message::Ping {
                            text: "cancel".into(),
                        },
                    )),
                )
                .await
                .is_err()
            );
            assert_eq!(client.pending.len(), 0);
        }
        drop(client);
        assert!(drain.await.unwrap() > 0);
    })
    .await
    .expect("cancel/drop must not leak reader and writer tasks");
}
