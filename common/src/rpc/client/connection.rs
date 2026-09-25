//! RPC 客户端连接的后台收发任务与入站消息分发。

use std::sync::{Arc, Mutex};

use tokio::{
    io::{ReadHalf, WriteHalf},
    net::windows::named_pipe::NamedPipeClient,
    sync::{broadcast, mpsc, watch},
};

use crate::message::{Envelope, PeerRole};

use super::{
    super::{
        RpcError, codec,
        limits::{protocol, runtime},
        read_frame, write_frame,
    },
    pending::PendingRequests,
};

/// 客户端公开 API 持有的发送端、订阅源和后台任务。
pub(super) struct ClientConnection {
    pub outbound: mpsc::Sender<Envelope>,
    pub closed: watch::Sender<bool>,
    pub incoming: broadcast::Sender<Envelope>,
    pub tasks: Arc<ClientTasks>,
}

/// 共享后台任务所有权；最后一个客户端克隆消失时中止管道任务。
pub(super) struct ClientTasks(Mutex<Vec<tokio::task::JoinHandle<()>>>);

impl ClientTasks {
    pub(super) async fn abort_all(&self) {
        let tasks = std::mem::take(&mut *self.0.lock().unwrap_or_else(|p| p.into_inner()));
        for task in tasks {
            task.abort();
            let _ = task.await;
        }
    }
}

impl Drop for ClientTasks {
    fn drop(&mut self) {
        for task in self.0.get_mut().unwrap_or_else(|p| p.into_inner()) {
            task.abort();
        }
    }
}

/// 接管一条已连接管道并启动 reader/writer；所有出口共享同一个关闭信号。
pub(super) fn spawn(
    pipe: NamedPipeClient,
    role: PeerRole,
    pending: PendingRequests,
) -> ClientConnection {
    let (reader, writer) = tokio::io::split(pipe);
    let (outbound, outbound_rx) = mpsc::channel(runtime::CLIENT_OUTBOUND_CAPACITY);
    let (closed, writer_closed) = watch::channel(false);
    let reader_closed = closed.subscribe();
    let (incoming, _) = broadcast::channel(runtime::CLIENT_INCOMING_CAPACITY);

    let writer_task = tokio::spawn(run_writer(
        writer,
        role,
        outbound_rx,
        writer_closed,
        pending.clone(),
        closed.clone(),
    ));
    let reader_task = tokio::spawn(run_reader(
        reader,
        reader_closed,
        pending,
        closed.clone(),
        incoming.clone(),
    ));

    ClientConnection {
        outbound,
        closed,
        incoming,
        tasks: Arc::new(ClientTasks(Mutex::new(vec![reader_task, writer_task]))),
    }
}

async fn run_writer(
    mut writer: WriteHalf<NamedPipeClient>,
    role: PeerRole,
    mut outbound: mpsc::Receiver<Envelope>,
    mut closed: watch::Receiver<bool>,
    pending: PendingRequests,
    close_signal: watch::Sender<bool>,
) {
    tokio::select! {
        _ = closed.changed() => {}
        _ = async {
            if codec::write_hello(&mut writer, role, std::process::id() as u64).await.is_err() {
                return;
            }
            loop {
                let envelope = match outbound.recv().await {
                    Some(envelope) => envelope,
                    None => break,
                };
                if !matches!(
                    tokio::time::timeout(
                        runtime::FRAME_WRITE_TIMEOUT,
                        write_frame(&mut writer, &envelope),
                    )
                    .await,
                    Ok(Ok(()))
                ) {
                    break;
                }
            }
        } => {}
    }
    close_connection(&pending, &close_signal);
}

async fn run_reader(
    mut reader: ReadHalf<NamedPipeClient>,
    mut closed: watch::Receiver<bool>,
    pending: PendingRequests,
    close_signal: watch::Sender<bool>,
    incoming: broadcast::Sender<Envelope>,
) {
    tokio::select! {
        _ = closed.changed() => {}
        _ = async {
            if codec::read_hello(&mut reader).await.is_err() {
                return;
            }
            loop {
                let frame = match read_frame(&mut reader).await {
                    Ok(Some(frame)) => frame,
                    Ok(None) | Err(_) => break,
                };
                let envelope = match codec::decode(&frame).and_then(|frame| {
                    if matches!(frame.body, Some(crate::message::rpc_frame::Body::Request(_))) {
                        return Err(RpcError::Protocol(
                            "server sent a request on a client endpoint".into(),
                        ));
                    }
                    codec::unpack(frame)
                }) {
                    Ok(envelope) => envelope,
                    Err(_) => break,
                };
                dispatch_incoming(envelope, &pending, &incoming);
            }
        } => {}
    }
    close_connection(&pending, &close_signal);
}

fn dispatch_incoming(
    envelope: Envelope,
    pending: &PendingRequests,
    incoming: &broadcast::Sender<Envelope>,
) {
    // 先发布再完成 request，使需要统一入站顺序的组件可在请求 future 醒来前观察响应。
    let _ = incoming.send(envelope.clone());
    if envelope.request_id == protocol::EVENT_REQUEST_ID {
        return;
    }
    pending.complete(envelope);
}

fn close_connection(pending: &PendingRequests, closed: &watch::Sender<bool>) {
    pending.close();
    closed.send_replace(true);
}
