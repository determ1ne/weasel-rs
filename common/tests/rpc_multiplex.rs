use std::time::Duration;

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
use weasel_common::{
    message::{
        ContextAction, ContextCommand, ContextToken, Envelope, InputOpened, KeyEventResponse,
        envelope::Payload,
    },
    rpc::{RpcClient, RpcServer},
};

#[tokio::test]
async fn one_pipe_opens_multiple_contexts_and_destroy_reopens_only_its_context() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let pipe = format!(r"\\.\pipe\weasel-multiplex-{}", std::process::id());
        let server = RpcServer::new(&pipe);
        server.bind().await.unwrap();
        let client = RpcClient::connect(&pipe).await.unwrap();
        let connection = server.accept().await.unwrap();
        let handler = tokio::spawn(async move {
            let mut opened = Vec::new();
            let mut commands = 0;
            while commands < 5 {
                let (request, _lease) = connection.recv_tracked().await.unwrap().unwrap();
                let payload = match request.payload.unwrap() {
                    Payload::OpenInput(open) => {
                        opened.push(open.token.unwrap().context_id);
                        Payload::InputOpened(InputOpened { token: open.token })
                    }
                    Payload::ContextCommand(command) => {
                        commands += 1;
                        Payload::KeyEventResponse(KeyEventResponse {
                            token: command.token,
                            revision: commands,
                            ..Default::default()
                        })
                    }
                    _ => panic!("unexpected request"),
                };
                connection
                    .send(&Envelope {
                        request_id: request.request_id,
                        payload: Some(payload),
                    })
                    .await
                    .unwrap();
            }
            opened
        });
        for (context_id, action) in [
            (1, ContextAction::Focus),
            (2, ContextAction::Focus),
            (1, ContextAction::Focus),
            (1, ContextAction::Destroy),
            (1, ContextAction::Focus),
        ] {
            let token = ContextToken {
                context_id,
                connection_epoch: 1,
                generation: 1,
            };
            let response = client
                .context_command(ContextCommand {
                    token: Some(token),
                    action: action as i32,
                    ascii_mode: None,
                })
                .await
                .unwrap();
            assert_eq!(response.token, Some(token));
        }
        assert_eq!(handler.await.unwrap(), [1, 2, 1]);
        client.disconnected().await;
        assert!(!client.is_connected());
    })
    .await
    .unwrap();
}
