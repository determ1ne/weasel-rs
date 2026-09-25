//! 单条服务端连接的 Named Pipe 收发任务。

use std::sync::Arc;

use tokio::{
    io::{ReadHalf, WriteHalf},
    net::windows::named_pipe::NamedPipeServer,
    sync::{mpsc, oneshot, watch},
};

use crate::message::{Envelope, PeerRole};

use super::{
    super::{RpcError, codec, limits::runtime, read_frame, write_frame},
    policy::{validate_business, validate_peer_role},
};

use crate::rpc::activity::Activity;

pub(super) type Outgoing = (Envelope, oneshot::Sender<Result<(), RpcError>>);

/// 交还给 [`super::RpcConnection`] 的连接状态和后台任务。
pub(super) struct ConnectionParts {
    pub client_executable: Option<String>,
    pub activity: Arc<Activity>,
    pub incoming: mpsc::Receiver<Result<Envelope, RpcError>>,
    pub outgoing: mpsc::Sender<Outgoing>,
    pub closed: watch::Sender<bool>,
    pub tasks: Vec<tokio::task::JoinHandle<()>>,
}

/// 接管已连接管道，并为每个方向各启动一个任务。
pub(super) fn spawn(
    pipe: NamedPipeServer,
    instance_id: u64,
    role: PeerRole,
    allow_unspecified_peer: bool,
) -> ConnectionParts {
    let client_executable = client_executable(&pipe);
    let (reader, writer) = tokio::io::split(pipe);
    let (incoming_tx, incoming) = mpsc::channel(runtime::SERVER_INCOMING_CAPACITY);
    let (outgoing, outgoing_rx) = mpsc::channel::<Outgoing>(runtime::SERVER_OUTGOING_CAPACITY);
    let (closed, reader_closed) = watch::channel(false);
    let writer_closed = closed.subscribe();
    let activity = Arc::new(Activity::default());

    let reader_task = tokio::spawn(run_reader(
        reader,
        role,
        allow_unspecified_peer,
        incoming_tx,
        reader_closed,
        activity.clone(),
        closed.clone(),
    ));
    let writer_task = tokio::spawn(run_writer(
        writer,
        role,
        instance_id,
        outgoing_rx,
        writer_closed,
        activity.clone(),
        closed.clone(),
    ));

    ConnectionParts {
        client_executable,
        activity,
        incoming,
        outgoing,
        closed,
        tasks: vec![reader_task, writer_task],
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_reader(
    mut reader: ReadHalf<NamedPipeServer>,
    local_role: PeerRole,
    allow_unspecified_peer: bool,
    incoming: mpsc::Sender<Result<Envelope, RpcError>>,
    mut closed: watch::Receiver<bool>,
    activity: Arc<Activity>,
    close_signal: watch::Sender<bool>,
) {
    tokio::select! {
        _ = closed.changed() => {}
        _ = async {
            let peer_role = match codec::read_hello(&mut reader)
                .await
                .and_then(|hello| {
                    validate_peer_role(local_role, hello.role, allow_unspecified_peer)
                })
            {
                Ok(peer_role) => peer_role,
                Err(error) => {
                    let _ = incoming.send(Err(error)).await;
                    return;
                }
            };
            loop {
                let result = match read_frame(&mut reader).await {
                    Ok(Some(bytes)) => codec::decode(&bytes)
                        .and_then(codec::unpack)
                        .and_then(|message| validate_business(local_role, peer_role, message)),
                    Ok(None) => break,
                    Err(error) => Err(error),
                };
                let failed = result.is_err();
                if result.is_ok() && !activity.request() {
                    break;
                }
                if incoming.send(result).await.is_err() || failed {
                    break;
                }
            }
        } => {}
    }
    close_signal.send_replace(true);
    activity.notify();
}

async fn run_writer(
    mut writer: WriteHalf<NamedPipeServer>,
    role: PeerRole,
    instance_id: u64,
    mut outgoing: mpsc::Receiver<Outgoing>,
    mut closed: watch::Receiver<bool>,
    activity: Arc<Activity>,
    close_signal: watch::Sender<bool>,
) {
    tokio::select! {
        _ = closed.changed() => {}
        _ = async {
            // 不等待读取对端 hello 就立即发送本端 hello，使 accept 后尚未启动 reader 的客户端
            // 也能继续完成握手。
            if tokio::time::timeout(
                runtime::HANDSHAKE_TIMEOUT,
                codec::write_hello(&mut writer, role, instance_id),
            )
            .await
            .unwrap_or(Err(RpcError::Timeout))
            .is_err()
            {
                return;
            }
            while let Some((message, acknowledgement)) = outgoing.recv().await {
                let result = tokio::time::timeout(
                    runtime::FRAME_WRITE_TIMEOUT,
                    write_frame(&mut writer, &message),
                )
                .await
                .unwrap_or(Err(RpcError::Timeout));
                let failed = result.is_err();
                activity.finish_write();
                let _ = acknowledgement.send(result);
                if failed {
                    break;
                }
            }
        } => {}
    }
    close_signal.send_replace(true);
    activity.notify();
}

fn client_executable(pipe: &NamedPipeServer) -> Option<String> {
    use crate::bindings::*;
    use std::os::windows::io::AsRawHandle;

    unsafe {
        let mut pid = 0;
        if !GetNamedPipeClientProcessId(HANDLE(pipe.as_raw_handle()), &mut pid).as_bool() {
            return None;
        }
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION as u32, false, pid);
        if process.0.is_null() {
            return None;
        }
        let mut path = [0u16; runtime::PROCESS_PATH_BUFFER_LEN];
        let mut len = path.len() as u32;
        let result = QueryFullProcessImageNameW(
            process,
            0,
            windows_strings::PWSTR(path.as_mut_ptr()),
            &mut len,
        );
        let _ = CloseHandle(process);
        if !result.as_bool() {
            return None;
        }
        let path = String::from_utf16(path.get(..len as usize)?).ok()?;
        Some(path.rsplit(['\\', '/']).next()?.to_owned())
    }
}
