use std::time::Duration;

use weasel_common::{
    message::envelope::Payload,
    rpc::{RpcClient, RpcError, RpcServer},
};

#[tokio::test(flavor = "current_thread")]
async fn key_replies_push_commits_and_context_commands_share_wire_order() {
    use weasel_common::message::{
        ContextAction, ContextCommand, ContextToken, KeyEvent, KeyEventResponse,
    };
    tokio::time::timeout(Duration::from_secs(3), async {
        let name = format!(r"\\.\pipe\weaselrs-ordered-context-{}", std::process::id());
        let server = RpcServer::new(&name);
        let accept = tokio::spawn(async move { (server.accept().await.unwrap(), server) });
        let client = connect_with_retry(&name).await.unwrap();
        let (connection, _server) = accept.await.unwrap();
        let token = ContextToken {
            context_id: 8,
            connection_epoch: 6,
            generation: 1,
        };
        let expected = token;
        let serve = tokio::spawn(async move {
            let key = receive_request(&connection).await;
            match key.payload {
                Some(Payload::KeyEvent(event)) => assert_eq!(event.token, Some(expected)),
                _ => panic!("expected key"),
            }
            connection
                .send_key_event_response(
                    0,
                    KeyEventResponse {
                        token: Some(expected),
                        revision: 1,
                        commit_text: "提交".into(),
                        state_updated: true,
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            connection
                .send_key_event_response(
                    key.request_id,
                    KeyEventResponse {
                        token: Some(expected),
                        revision: 2,
                        eaten: true,
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            let control = connection.recv().await.unwrap().unwrap();
            let command = match control.payload {
                Some(Payload::ContextCommand(command)) => command,
                _ => panic!("expected control"),
            };
            assert_eq!(command.action, ContextAction::Cancel as i32);
            assert_eq!(command.token.as_ref().unwrap().generation, 2);
            connection
                .send_key_event_response(
                    control.request_id,
                    KeyEventResponse {
                        token: command.token,
                        revision: 3,
                        state_updated: true,
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            connection
        });
        let mut ordered = client.subscribe_key_responses();
        let mut unsolicited = client.subscribe_key_updates();
        assert!(
            client
                .process_translated_key(KeyEvent {
                    token: Some(token),
                    keycode: Some(97),
                    ..Default::default()
                })
                .await
                .unwrap()
                .eaten
        );
        client
            .context_command(ContextCommand {
                token: Some(ContextToken {
                    generation: 2,
                    ..token
                }),
                action: ContextAction::Cancel as i32,
            })
            .await
            .unwrap();
        let first = ordered.recv().await.unwrap();
        assert_eq!(first.commit_text, "提交");
        assert_eq!(first.revision, 1);
        assert_eq!(ordered.recv().await.unwrap().revision, 2);
        assert_eq!(ordered.recv().await.unwrap().revision, 3);
        assert_eq!(unsolicited.recv().await.unwrap().revision, 1);
        assert!(unsolicited.try_recv().is_err());
        drop(serve.await.unwrap());
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn disconnected_client_rejects_new_requests_and_reconnects() {
    tokio::time::timeout(Duration::from_secs(3), async {
        let name = format!(r"\\.\pipe\weaselrs-disconnect-{}", std::process::id());
        let server = RpcServer::new(&name);
        let accept = tokio::spawn(async move { (server.accept().await.unwrap(), server) });
        let client = connect_with_retry(&name).await.unwrap();
        let (connection, server) = accept.await.unwrap();
        drop(connection);
        while client.is_connected() {
            tokio::task::yield_now().await;
        }
        assert!(matches!(
            client.ping("after EOF").await,
            Err(RpcError::Disconnected)
        ));
        assert!(matches!(
            client
                .process_translated_key(weasel_common::message::KeyEvent {
                    keycode: Some(97),
                    ..Default::default()
                })
                .await,
            Err(RpcError::Disconnected)
        ));
        assert!(matches!(
            client.shutdown("after EOF").await,
            Err(RpcError::Disconnected)
        ));
        let fresh = RpcClient::connect(&name).await.unwrap();
        let connection = server.accept().await.unwrap();
        let reply = tokio::spawn(async move {
            let request = connection.recv().await.unwrap().unwrap();
            connection
                .send_pong(request.request_id, "fresh session")
                .await
                .unwrap();
            connection
        });
        assert_eq!(
            fresh.ping("reconnected").await.unwrap().text,
            "fresh session"
        );
        drop(reply.await.unwrap());
    })
    .await
    .expect("disconnect/reconnect must not hang");
}

#[tokio::test(flavor = "current_thread")]
async fn closing_connection_wakes_pending_request() {
    tokio::time::timeout(Duration::from_secs(3), async {
        let name = format!(r"\\.\pipe\weaselrs-pending-{}", std::process::id());
        let server = RpcServer::new(&name);
        let accept = tokio::spawn(async move { (server.accept().await.unwrap(), server) });
        let client = connect_with_retry(&name).await.unwrap();
        let (connection, _server) = accept.await.unwrap();
        let caller = client.clone();
        let pending = tokio::spawn(async move { caller.ping("no reply").await });
        connection.recv().await.unwrap().unwrap();
        client.disconnect().await;
        assert!(matches!(
            pending.await.unwrap(),
            Err(RpcError::Disconnected)
        ));
        assert!(matches!(
            client.ping("closed").await,
            Err(RpcError::Disconnected)
        ));
    })
    .await
    .expect("pending request must be cancelled");
}

#[tokio::test(flavor = "current_thread")]
async fn cancelled_accept_retains_instance_and_shutdown_can_connect_alongside_tip() {
    tokio::time::timeout(Duration::from_secs(3), async {
        let name = format!(r"\\.\pipe\weaselrs-concurrent-{}", std::process::id());
        let server = RpcServer::new(&name);
        // Emulate another branch winning select! while the listener is idle.
        assert!(
            tokio::time::timeout(Duration::from_millis(5), server.accept())
                .await
                .is_err()
        );
        let tip = RpcClient::connect(&name).await.unwrap();
        let tip_connection = server.accept().await.unwrap();
        let broker = RpcClient::connect(&name).await.unwrap();
        let broker_connection = server.accept().await.unwrap();
        let broker_task = tokio::spawn(async move {
            let request = broker_connection.recv().await.unwrap().unwrap();
            assert!(matches!(request.payload, Some(Payload::Shutdown(_))));
            broker_connection
                .send_shutdown_response(
                    request.request_id,
                    weasel_common::message::ShutdownResponse {
                        accepted: true,
                        message: "bye".into(),
                    },
                )
                .await
                .unwrap();
            // Retain the pipe until the caller has read its acknowledgement.
            broker_connection
        });
        assert!(
            broker
                .shutdown("deploy with live TIP")
                .await
                .unwrap()
                .accepted
        );
        let retained = broker_task.await.unwrap();
        drop((retained, tip_connection, tip));
    })
    .await
    .expect("concurrent shutdown timed out");
}

async fn connect_with_retry(pipe_name: &str) -> Result<RpcClient, RpcError> {
    for _ in 0..100 {
        match RpcClient::connect(pipe_name).await {
            Ok(client) => return Ok(client),
            Err(_) => tokio::task::yield_now().await,
        }
    }

    Err(RpcError::Disconnected)
}

#[tokio::test(flavor = "current_thread")]
async fn unsolicited_candidate_result_is_delivered_without_a_key_request() {
    tokio::time::timeout(Duration::from_secs(3), async {
        let name = format!(r"\\.\pipe\weaselrs-push-{}", std::process::id());
        let server = RpcServer::new(&name);
        let accept = tokio::spawn(async move { (server.accept().await.unwrap(), server) });
        let client = connect_with_retry(&name).await.unwrap();
        let mut updates = client.subscribe_key_updates();
        let (connection, _server) = accept.await.unwrap();
        connection
            .send_key_event_response(
                0,
                weasel_common::message::KeyEventResponse {
                    state_updated: true,
                    commit_text: "候选提交".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(updates.recv().await.unwrap().commit_text, "候选提交");
        connection
            .send_key_event_response(
                0,
                weasel_common::message::KeyEventResponse {
                    open_emoji_panel: true,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(updates.recv().await.unwrap().open_emoji_panel);
    })
    .await
    .expect("unsolicited responses must not be discarded");
}

#[tokio::test(flavor = "current_thread")]
async fn named_pipe_ping_round_trip_and_events() {
    let pipe_name = format!(r"\\.\pipe\weaselrs-test-{}", std::process::id());
    let server = RpcServer::new(pipe_name.clone());

    let server_task = tokio::spawn(async move {
        let connection = server.accept().await?;
        connection
            .send_log_event("info", "test server connected")
            .await?;

        let envelope = receive_request(&connection).await;
        match envelope.payload {
            Some(Payload::Ping(ping)) => {
                assert_eq!(ping.text, "integration ping");
                connection
                    .send_pong(envelope.request_id, "integration pong")
                    .await?;
            }
            payload => panic!("unexpected payload: {payload:?}"),
        }

        let envelope = receive_request(&connection).await;
        match envelope.payload {
            Some(Payload::KeyEvent(key_event)) => {
                assert_eq!(key_event.virtual_key, 0x41);
                assert_eq!(key_event.lparam, 0x1234);
                assert!(!key_event.key_up);
                assert!(!key_event.test);
                connection
                    .send_key_event_response(
                        envelope.request_id,
                        weasel_common::message::KeyEventResponse {
                            eaten: false,
                            state_updated: true,
                            composition: "a".to_owned(),
                            candidates: vec![weasel_common::message::Candidate {
                                text: "啊".to_owned(),
                                comment: "a".to_owned(),
                            }],
                            ..Default::default()
                        },
                    )
                    .await?;
            }
            payload => panic!("unexpected payload: {payload:?}"),
        }

        let envelope = connection.recv().await?.ok_or(RpcError::Disconnected)?;
        match envelope.payload {
            Some(Payload::Shutdown(request)) => {
                assert_eq!(request.reason, "test shutdown");
                connection
                    .send_shutdown_response(
                        envelope.request_id,
                        weasel_common::message::ShutdownResponse {
                            accepted: true,
                            message: "bye".to_owned(),
                        },
                    )
                    .await?;
            }
            payload => panic!("unexpected payload: {payload:?}"),
        }

        Ok::<(), RpcError>(())
    });

    let client = tokio::time::timeout(Duration::from_secs(2), connect_with_retry(&pipe_name))
        .await
        .expect("connecting to the test Named Pipe timed out")
        .expect("the test Named Pipe client failed to connect");
    let mut events = client.subscribe_events();

    let response = tokio::time::timeout(Duration::from_secs(2), client.ping("integration ping"))
        .await
        .expect("waiting for the RPC response timed out")
        .expect("the RPC ping failed");
    assert_eq!(response.text, "integration pong");

    let response = tokio::time::timeout(
        Duration::from_secs(2),
        client.process_translated_key(weasel_common::message::KeyEvent {
            virtual_key: 0x41,
            lparam: 0x1234,
            keycode: Some(97),
            token: Some(weasel_common::message::ContextToken {
                context_id: 1,
                connection_epoch: 1,
                generation: 1,
            }),
            ..Default::default()
        }),
    )
    .await
    .expect("waiting for the key event response timed out")
    .expect("the key event request failed");
    assert!(!response.eaten);
    assert_eq!(response.composition, "a");
    assert_eq!(response.candidates[0].text, "啊");

    let event = tokio::time::timeout(Duration::from_secs(2), events.recv())
        .await
        .expect("waiting for the server event timed out")
        .expect("the server event channel closed");
    assert_eq!(event.level, "info");
    assert_eq!(event.text, "test server connected");

    let response = tokio::time::timeout(Duration::from_secs(2), client.shutdown("test shutdown"))
        .await
        .expect("waiting for the shutdown response timed out")
        .expect("the shutdown request failed");
    assert!(response.accepted);
    assert_eq!(response.message, "bye");

    drop(client);
    server_task
        .await
        .expect("the test server task panicked")
        .expect("the test server failed");
}

async fn receive_request(
    connection: &weasel_common::rpc::RpcConnection,
) -> weasel_common::message::Envelope {
    loop {
        let request = connection.recv().await.unwrap().unwrap();
        if let Some(Payload::OpenInput(open)) = request.payload.as_ref() {
            connection
                .send(&weasel_common::message::Envelope {
                    request_id: request.request_id,
                    payload: Some(Payload::InputOpened(weasel_common::message::InputOpened {
                        token: open.token,
                    })),
                })
                .await
                .unwrap();
        } else {
            return request;
        }
    }
}
