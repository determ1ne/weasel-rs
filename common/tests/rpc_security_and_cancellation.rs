#![cfg(windows)]

use prost::Message;
use std::{os::windows::io::AsRawHandle, time::Duration};
#[path = "../src/bindings.rs"]
mod bindings;
use bindings::*;
use tokio::{
    io::AsyncWriteExt,
    net::windows::named_pipe::{ClientOptions, NamedPipeClient, ServerOptions},
    time::timeout,
};
use weasel_common::{
    framing,
    message::{Hello, PeerRole, Ping, Request, RpcFrame, request::Operation, rpc_frame::Body},
    platform::RuntimeIdentity,
    rpc::{self, RpcConnection, RpcError, RpcServer},
};

const DEADLINE: Duration = Duration::from_secs(5);
const CANCEL: Duration = Duration::from_millis(10);

fn name(component: &str) -> String {
    RuntimeIdentity::current()
        .unwrap()
        .pipe_name(&format!("test-{component}-{}", std::process::id()))
        .unwrap()
}

async fn pair(server: &RpcServer) -> (NamedPipeClient, RpcConnection) {
    // Poll and cancel accept to create the first instance without a client.
    assert!(timeout(CANCEL, server.accept()).await.is_err());
    let mut client = ClientOptions::new().open(server.pipe_name()).unwrap();
    let connection = server.accept().await.unwrap();
    handshake(&mut client).await;
    (client, connection)
}

fn frame(id: u64) -> Vec<u8> {
    framing::encode(&RpcFrame {
        protocol_version: 2,
        body: Some(Body::Request(Request {
            id,
            operation: Some(Operation::Ping(Ping {
                text: "synthetic-test".into(),
            })),
        })),
    })
    .unwrap()
}

async fn handshake(client: &mut NamedPipeClient) -> u64 {
    handshake_as(client, PeerRole::Tip, PeerRole::Server).await
}

async fn handshake_as(client: &mut NamedPipeClient, peer: PeerRole, local: PeerRole) -> u64 {
    // Read first to prove the server writes hello without awaiting ours.
    let bytes = framing::read(client).await.unwrap().unwrap();
    let hello = RpcFrame::decode(bytes.as_slice()).unwrap();
    assert_eq!(hello.protocol_version, 2);
    let Some(Body::Hello(hello)) = hello.body else {
        panic!("server hello required")
    };
    assert_eq!(hello.role, local as i32);
    assert_ne!(hello.instance_id, 0);
    framing::write(
        client,
        &RpcFrame {
            protocol_version: 2,
            body: Some(Body::Hello(Hello {
                role: peer as i32,
                instance_id: 1,
            })),
        },
    )
    .await
    .unwrap();
    hello.instance_id
}

#[tokio::test]
async fn strict_listeners_reject_wrong_hello_roles() {
    timeout(DEADLINE, async {
        for (index, local, peer) in [
            (0, PeerRole::Renderer, PeerRole::Tip),
            (1, PeerRole::Renderer, PeerRole::Unspecified),
            (2, PeerRole::Server, PeerRole::Renderer),
            (3, PeerRole::Server, PeerRole::Server),
            (4, PeerRole::Server, PeerRole::Unspecified),
        ] {
            let server = RpcServer::with_role(name(&format!("role-hello-{index}")), local);
            assert!(timeout(CANCEL, server.accept()).await.is_err());
            let mut client = ClientOptions::new().open(server.pipe_name()).unwrap();
            let connection = server.accept().await.unwrap();
            handshake_as(&mut client, peer, local).await;
            assert!(matches!(
                connection.recv().await,
                Err(RpcError::Protocol(_))
            ));
            assert!(connection.recv().await.unwrap().is_none());
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn role_operation_allowlists_and_response_rejection() {
    use m::event::Notification as N;
    use weasel_common::message as m;
    timeout(DEADLINE, async {
        let token = Some(m::ContextToken {
            context_id: 1,
            connection_epoch: 1,
            generation: 1,
        });
        let requests = [
            Operation::Ping(Ping::default()),
            Operation::Shutdown(m::Shutdown::default()),
            Operation::OpenInput(m::OpenInput { token }),
            Operation::Key(m::InputKey {
                token,
                keycode: 65,
                ..Default::default()
            }),
            Operation::Context(m::ContextCommand {
                token,
                action: m::ContextAction::Cancel as i32,
            }),
        ];
        let mut bodies: Vec<_> = requests
            .into_iter()
            .map(|operation| {
                Body::Request(Request {
                    id: 1,
                    operation: Some(operation),
                })
            })
            .collect();
        bodies.extend([
            Body::Event(m::Event {
                notification: Some(N::Layout(m::LayoutUpdate::default())),
            }),
            Body::Event(m::Event {
                notification: Some(N::Diagnostic(m::LogEvent::default())),
            }),
            Body::Event(m::Event {
                notification: Some(N::Render(m::RenderSnapshot::default())),
            }),
            Body::Response(m::Response {
                id: 1,
                result: Some(m::response::Result::Pong(m::Pong::default())),
            }),
        ]);
        for (role_index, local, peer, allowed) in [
            (0, PeerRole::Server, PeerRole::Broker, vec![0, 1]),
            (1, PeerRole::Server, PeerRole::Tip, vec![0, 2, 3, 4, 5, 6]),
            (2, PeerRole::Renderer, PeerRole::Server, vec![0, 7]),
            (3, PeerRole::Renderer, PeerRole::Broker, vec![0, 1]),
        ] {
            for (index, body) in bodies.iter().enumerate() {
                let server =
                    RpcServer::with_role(name(&format!("role-op-{role_index}-{index}")), local);
                assert!(timeout(CANCEL, server.accept()).await.is_err());
                let mut client = ClientOptions::new().open(server.pipe_name()).unwrap();
                let connection = server.accept().await.unwrap();
                handshake_as(&mut client, peer, local).await;
                framing::write(
                    &mut client,
                    &RpcFrame {
                        protocol_version: 2,
                        body: Some(body.clone()),
                    },
                )
                .await
                .unwrap();
                if allowed.contains(&index) {
                    if index == 5 {
                        // Authorized layout bypasses the ordered input FIFO.
                        while connection.take_layout_for(None).is_none() {
                            tokio::task::yield_now().await;
                        }
                        continue;
                    }
                    assert!(
                        connection.recv().await.unwrap().is_some(),
                        "{peer:?} operation {index}"
                    );
                } else {
                    assert!(
                        matches!(connection.recv().await, Err(RpcError::Protocol(_))),
                        "{peer:?} operation {index}"
                    );
                    assert!(connection.recv().await.unwrap().is_none());
                }
            }
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn hello_instance_is_stable_across_connections() {
    timeout(DEADLINE, async {
        let server = RpcServer::new(name("hello-instance"));
        assert!(timeout(CANCEL, server.accept()).await.is_err());
        let mut first = ClientOptions::new().open(server.pipe_name()).unwrap();
        let _first_connection = server.accept().await.unwrap();
        let instance = handshake(&mut first).await;
        let mut second = ClientOptions::new().open(server.pipe_name()).unwrap();
        let _second_connection = server.accept().await.unwrap();
        assert_eq!(handshake(&mut second).await, instance);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn business_before_hello_is_rejected_and_closes_connection() {
    timeout(DEADLINE, async {
        let server = RpcServer::new(name("missing-hello"));
        assert!(timeout(CANCEL, server.accept()).await.is_err());
        let mut client = ClientOptions::new().open(server.pipe_name()).unwrap();
        let connection = server.accept().await.unwrap();
        // Consume the server hello, but send a business request instead of ours.
        assert!(framing::read(&mut client).await.unwrap().is_some());
        client.write_all(&frame(1)).await.unwrap();
        assert!(matches!(
            connection.recv().await,
            Err(RpcError::Protocol(_))
        ));
        assert!(connection.recv().await.unwrap().is_none());
        assert!(matches!(
            connection.send_pong(1, "must not send").await,
            Err(RpcError::Disconnected)
        ));
    })
    .await
    .unwrap();
}

#[test]
fn defaults_use_process_user_and_logon_identity() {
    let identity = RuntimeIdentity::current().unwrap();
    assert_eq!(
        rpc::default_pipe_name(),
        identity.pipe_name("server").unwrap()
    );
    assert_eq!(
        rpc::default_renderer_pipe_name(),
        identity.pipe_name("renderer").unwrap()
    );
    assert_eq!(
        rpc::try_default_pipe_name().unwrap(),
        rpc::default_pipe_name()
    );
    assert_eq!(
        rpc::try_default_renderer_pipe_name().unwrap(),
        rpc::default_renderer_pipe_name()
    );
    assert_ne!(rpc::default_pipe_name(), rpc::default_renderer_pipe_name());
}

#[tokio::test]
async fn refuses_preexisting_listener_and_competing_rpc_listener() {
    timeout(DEADLINE, async {
        let name = name("first-instance");
        let occupied = ServerOptions::new().create(&name).unwrap();
        let server = RpcServer::new(&name);
        assert!(matches!(server.accept().await, Err(RpcError::Io(e)) if e.kind() == std::io::ErrorKind::PermissionDenied));
        drop(occupied);
        let (_client, _connection) = pair(&server).await;
        let competitor = RpcServer::new(&name);
        assert!(matches!(competitor.accept().await, Err(RpcError::Io(e)) if e.kind() == std::io::ErrorKind::PermissionDenied));
        // First-instance protection does not prevent our own additional clients.
        let _second = ClientOptions::new().open(&name).unwrap();
        let _second_connection = server.accept().await.unwrap();
    }).await.unwrap();
}

fn pipe_security(client: &NamedPipeClient, flags: u32) -> String {
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    // Inspect the actual kernel security descriptor, including labels when requested,
    // object after the creation helper's temporary descriptor has been dropped.
    assert_eq!(
        unsafe {
            GetSecurityInfo(
                HANDLE(client.as_raw_handle()),
                SE_KERNEL_OBJECT,
                SECURITY_INFORMATION(flags),
                None,
                None,
                None,
                None,
                Some(&mut descriptor),
            )
        },
        0
    );
    let mut text = windows_core::PWSTR::null();
    let converted = unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor,
            SDDL_REVISION_1 as u32,
            SECURITY_INFORMATION(flags),
            &mut text,
            None,
        )
    };
    unsafe {
        LocalFree(HANDLE(descriptor.0));
    }
    assert!(converted.as_bool());
    unsafe {
        let mut len = 0;
        while *text.0.add(len) != 0 {
            len += 1;
        }
        let result = String::from_utf16_lossy(std::slice::from_raw_parts(text.0, len));
        LocalFree(HANDLE(text.0.cast()));
        result
    }
}

#[tokio::test]
async fn every_input_instance_has_appcontainer_access_and_low_integrity() {
    timeout(DEADLINE, async {
        let server = RpcServer::new(name("acl"));
        // The kernel maps GENERIC_ALL to the pipe's FILE_ALL_ACCESS mask.
        let expected = format!(
            "D:P(D;;FA;;;NU)(A;;;;;OW)(A;;FA;;;SY)(A;;FA;;;BA)(A;;FA;;;AC)(A;;FA;;;{})S:(ML;;NX;;;LW)",
            RuntimeIdentity::current().unwrap().user_sid()
        );
        let (first, _first_connection) = pair(&server).await;
        let flags = (DACL_SECURITY_INFORMATION | LABEL_SECURITY_INFORMATION) as u32;
        // The kernel may add the SACL auto-inherited control flag; compare ACEs
        // exactly while allowing that bookkeeping difference.
        assert_eq!(pipe_security(&first, flags).replace("S:AI(", "S:("), expected);
        let second = ClientOptions::new().open(server.pipe_name()).unwrap();
        let _second_connection = server.accept().await.unwrap();
        assert_eq!(pipe_security(&second, flags).replace("S:AI(", "S:("), expected);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn renderer_pipe_keeps_its_logon_private_acl() {
    timeout(DEADLINE, async {
        let server = RpcServer::with_role(name("renderer-acl"), PeerRole::Renderer);
        assert!(timeout(CANCEL, server.accept()).await.is_err());
        let mut client = ClientOptions::new().open(server.pipe_name()).unwrap();
        let _connection = server.accept().await.unwrap();
        handshake_as(&mut client, PeerRole::Server, PeerRole::Renderer).await;
        assert_eq!(
            pipe_security(&client, DACL_SECURITY_INFORMATION as u32),
            format!(
                "D:P(D;;FA;;;NU)(A;;FA;;;{})",
                RuntimeIdentity::current().unwrap().logon_sid()
            )
        );
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn cancelling_recv_preserves_partial_header_body_and_following_frame() {
    timeout(DEADLINE, async {
        let server = RpcServer::new(name("cancel-recv"));
        let (mut client, connection) = pair(&server).await;
        for (id, split) in [(1, 1), (2, 3), (3, 5)] {
            let bytes = frame(id);
            client.write_all(&bytes[..split]).await.unwrap();
            // Let the persistent reader consume a partial frame, then cancel only
            // the public receiver repeatedly. No decoder state may be lost.
            for _ in 0..3 {
                assert!(timeout(CANCEL, connection.recv()).await.is_err());
            }
            let mut tail = bytes[split..].to_vec();
            tail.extend(frame(id + 100));
            client.write_all(&tail).await.unwrap();
            assert_eq!(connection.recv().await.unwrap().unwrap().request_id, id);
            assert_eq!(
                connection.recv().await.unwrap().unwrap().request_id,
                id + 100
            );
        }
        drop(client);
        assert!(connection.recv().await.unwrap().is_none());
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn truncated_frame_fails_only_its_connection() {
    timeout(DEADLINE, async {
        for (index, split) in [(0, 2), (1, 5)] {
            let server = RpcServer::new(name(&format!("truncated-{index}")));
            let (mut broken, broken_connection) = pair(&server).await;
            let mut healthy = ClientOptions::new().open(server.pipe_name()).unwrap();
            let healthy_connection = server.accept().await.unwrap();
            handshake(&mut healthy).await;
            broken.write_all(&frame(1)[..split]).await.unwrap();
            assert!(timeout(CANCEL, broken_connection.recv()).await.is_err());
            drop(broken);
            assert!(matches!(broken_connection.recv().await, Err(RpcError::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof));
            assert!(broken_connection.recv().await.unwrap().is_none());
            healthy.write_all(&frame(77)).await.unwrap();
            assert_eq!(healthy_connection.recv().await.unwrap().unwrap().request_id, 77);
        }
    }).await.unwrap();
}

#[tokio::test]
async fn simultaneous_connections_keep_frames_and_disconnects_isolated() {
    timeout(DEADLINE, async {
        let server = RpcServer::new(name("isolation"));
        let (mut first, first_connection) = pair(&server).await;
        let mut second = ClientOptions::new().open(server.pipe_name()).unwrap();
        let second_connection = server.accept().await.unwrap();
        handshake(&mut second).await;
        let bytes = frame(11);
        first.write_all(&bytes[..2]).await.unwrap();
        second.write_all(&frame(22)).await.unwrap();
        assert_eq!(
            second_connection.recv().await.unwrap().unwrap().request_id,
            22
        );
        assert!(timeout(CANCEL, first_connection.recv()).await.is_err());
        first.write_all(&bytes[2..]).await.unwrap();
        assert_eq!(
            first_connection.recv().await.unwrap().unwrap().request_id,
            11
        );
        drop(first);
        assert!(first_connection.recv().await.unwrap().is_none());
        second.write_all(&frame(33)).await.unwrap();
        assert_eq!(
            second_connection.recv().await.unwrap().unwrap().request_id,
            33
        );
    })
    .await
    .unwrap();
}
