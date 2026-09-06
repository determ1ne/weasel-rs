//! RPC client used by tip and other front-end components.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use tokio::{
    net::windows::named_pipe::ClientOptions,
    sync::{broadcast, mpsc, oneshot, watch},
};

use crate::message::{
    Envelope, KeyEvent, KeyEventResponse, LayoutUpdate, LogEvent, Ping, Pong, RenderSnapshot,
    RendererEvent, Shutdown, ShutdownResponse, envelope::Payload,
};

use super::{RpcError, read_frame, wire, write_frame};

type Pending = Arc<Mutex<Option<HashMap<u64, oneshot::Sender<Result<Envelope, RpcError>>>>>>;

async fn close_connection(pending: &Pending, closed: &watch::Sender<bool>) {
    // Registration and closing share one lock: requests cannot be inserted
    // after the final drain, even if the writer still has queue capacity.
    if let Some(requests) = pending.lock().unwrap_or_else(|p| p.into_inner()).take() {
        for (_, sender) in requests {
            let _ = sender.send(Err(RpcError::Disconnected));
        }
    }
    closed.send_replace(true);
}

#[cfg(test)]
mod tests {
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
            // More cancellations than the pending-map bound, with no replies.
            for _ in 0..80 {
                assert!(
                    tokio::time::timeout(Duration::from_millis(1), client.ping("cancel"))
                        .await
                        .is_err()
                );
                assert_eq!(client.pending.lock().unwrap().as_ref().unwrap().len(), 0);
            }
            drop(client);
            assert!(drain.await.unwrap() > 0);
        })
        .await
        .expect("cancel/drop must not leak reader and writer tasks");
    }
}

struct ClientTasks(Mutex<Vec<tokio::task::JoinHandle<()>>>);
impl Drop for ClientTasks {
    fn drop(&mut self) {
        for task in self.0.get_mut().unwrap_or_else(|p| p.into_inner()) {
            task.abort();
        }
    }
}
struct PendingCall {
    pending: Pending,
    id: u64,
}
impl Drop for PendingCall {
    fn drop(&mut self) {
        if let Some(requests) = self
            .pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_mut()
        {
            requests.remove(&self.id);
        }
    }
}

/// A connected bidirectional RPC client.
#[derive(Clone)]
pub struct RpcClient {
    outbound: mpsc::Sender<Envelope>,
    pending: Pending,
    closed: watch::Sender<bool>,
    events: broadcast::Sender<LogEvent>,
    render_snapshots: broadcast::Sender<RenderSnapshot>,
    renderer_events: broadcast::Sender<RendererEvent>,
    key_updates: broadcast::Sender<KeyEventResponse>,
    key_responses: broadcast::Sender<KeyEventResponse>,
    next_request_id: Arc<std::sync::atomic::AtomicU64>,
    tasks: Arc<ClientTasks>,
    input_open: Arc<tokio::sync::Mutex<bool>>,
}

impl RpcClient {
    /// Connect to a Named Pipe and start the background reader/writer tasks.
    pub async fn connect(pipe_name: impl AsRef<str>) -> Result<Self, RpcError> {
        Self::connect_as(pipe_name, crate::message::PeerRole::Unspecified).await
    }

    pub async fn connect_as(
        pipe_name: impl AsRef<str>,
        role: crate::message::PeerRole,
    ) -> Result<Self, RpcError> {
        let pipe = ClientOptions::new()
            .read(true)
            .write(true)
            .open(pipe_name.as_ref())?;
        let (mut reader, mut writer) = tokio::io::split(pipe);
        let (outbound, mut outbound_rx) = mpsc::channel(32);
        let pending: Pending = Arc::new(Mutex::new(Some(HashMap::new())));
        let (closed, mut writer_closed) = watch::channel(false);
        let mut reader_closed = closed.subscribe();
        let (events, _) = broadcast::channel(32);
        let (render_snapshots, _) = broadcast::channel(32);
        let (renderer_events, _) = broadcast::channel(32);
        let (key_updates, _) = broadcast::channel(32);
        let (key_responses, _) = broadcast::channel(64);
        let reader_key_responses = key_responses.clone();
        let reader_key_updates = key_updates.clone();

        let writer_pending = pending.clone();
        let writer_signal = closed.clone();
        let writer_task = tokio::spawn(async move {
            tokio::select! {
            _ = writer_closed.changed() => {},
            _ = async {
            if wire::write_hello(&mut writer, role, std::process::id() as u64).await.is_err() { return; }
            while let Some(envelope) = outbound_rx.recv().await {
                if !matches!(tokio::time::timeout(std::time::Duration::from_secs(2), write_frame(&mut writer, &envelope)).await, Ok(Ok(()))) {
                    break;
                }
            }
            } => {}
            }
            close_connection(&writer_pending, &writer_signal).await;
        });

        let reader_pending = Arc::clone(&pending);
        let reader_events = events.clone();
        let reader_render_snapshots = render_snapshots.clone();
        let reader_renderer_events = renderer_events.clone();
        let reader_signal = closed.clone();
        let reader_task = tokio::spawn(async move {
            tokio::select! {
            _ = reader_closed.changed() => {},
            _ = async {
            if wire::read_hello(&mut reader).await.is_err() { return; }
            loop {
                let frame = match read_frame(&mut reader).await {
                    Ok(Some(frame)) => frame,
                    Ok(None) | Err(_) => break,
                };
                let envelope = match wire::decode(&frame).and_then(|frame| {
                    if matches!(frame.body, Some(crate::message::rpc_frame::Body::Request(_))) {
                        return Err(RpcError::Protocol("server sent a request on a client endpoint".into()));
                    }
                    wire::unpack(frame)
                }) {
                    Ok(envelope) => envelope,
                    Err(_) => break,
                };

                // A single ordered stream for TIP document edits. Mixing direct
                // replies and unsolicited commits on different paths reorders them.
                if let Some(Payload::KeyEventResponse(response)) = envelope.payload.as_ref() {
                    crate::input_trace!("rpc.rx request={} token={:?} revision={} eaten={} state={} preedit_bytes={} commit_bytes={}", envelope.request_id, response.token, response.revision, response.eaten, response.state_updated, response.composition.len(), response.commit_text.len());
                    let _ = reader_key_responses.send(response.clone());
                }
                if envelope.request_id == 0
                    && let Some(Payload::KeyEventResponse(response)) = envelope.payload.as_ref() {
                        let _ = reader_key_updates.send(response.clone());
                        continue;
                    }
                if let Some(Payload::LogEvent(event)) = envelope.payload.as_ref() {
                    let _ = reader_events.send(event.clone());
                    continue;
                }
                if let Some(Payload::RenderSnapshot(snapshot)) = envelope.payload.as_ref() {
                    let _ = reader_render_snapshots.send(snapshot.clone());
                    continue;
                }
                if let Some(Payload::RendererEvent(event)) = envelope.payload.as_ref() {
                    let _ = reader_renderer_events.send(*event);
                    continue;
                }

                if let Some(sender) = reader_pending.lock().unwrap_or_else(|p| p.into_inner()).as_mut().and_then(|requests| requests.remove(&envelope.request_id)) {
                    let _ = sender.send(Ok(envelope));
                }
            }
            } => {}
            }
            close_connection(&reader_pending, &reader_signal).await;
        });

        Ok(Self {
            outbound,
            pending,
            closed,
            events,
            render_snapshots,
            renderer_events,
            key_updates,
            key_responses,
            next_request_id: Arc::new(std::sync::atomic::AtomicU64::new(1)),
            tasks: Arc::new(ClientTasks(Mutex::new(vec![reader_task, writer_task]))),
            input_open: Arc::new(tokio::sync::Mutex::new(false)),
        })
    }

    pub fn is_connected(&self) -> bool {
        !*self.closed.borrow()
    }

    pub fn subscribe_key_updates(&self) -> broadcast::Receiver<KeyEventResponse> {
        self.key_updates.subscribe()
    }

    pub fn subscribe_key_responses(&self) -> broadcast::Receiver<KeyEventResponse> {
        self.key_responses.subscribe()
    }

    /// Cancel outstanding calls and release both halves of the pipe.
    pub async fn disconnect(&self) {
        close_connection(&self.pending, &self.closed).await;
        let tasks = std::mem::take(&mut *self.tasks.0.lock().unwrap_or_else(|p| p.into_inner()));
        for task in tasks {
            task.abort();
            let _ = task.await;
        }
    }

    /// Subscribe to server-to-client log events.
    pub fn subscribe_events(&self) -> broadcast::Receiver<LogEvent> {
        self.events.subscribe()
    }

    pub fn subscribe_render_snapshots(&self) -> broadcast::Receiver<RenderSnapshot> {
        self.render_snapshots.subscribe()
    }

    pub fn subscribe_renderer_events(&self) -> broadcast::Receiver<RendererEvent> {
        self.renderer_events.subscribe()
    }

    pub async fn send_render_snapshot(&self, snapshot: RenderSnapshot) -> Result<(), RpcError> {
        if !self.is_connected() {
            return Err(RpcError::Disconnected);
        }
        self.outbound
            .try_send(Envelope {
                request_id: 0,
                payload: Some(Payload::RenderSnapshot(snapshot)),
            })
            .map_err(|error| self.queue_error(error))
    }

    pub async fn send_renderer_event(&self, event: RendererEvent) -> Result<(), RpcError> {
        if !self.is_connected() {
            return Err(RpcError::Disconnected);
        }
        self.outbound
            .try_send(Envelope {
                request_id: 0,
                payload: Some(Payload::RendererEvent(event)),
            })
            .map_err(|error| self.queue_error(error))
    }

    pub async fn send_layout_update(&self, update: LayoutUpdate) -> Result<(), RpcError> {
        if !self.is_connected() {
            return Err(RpcError::Disconnected);
        }
        self.outbound
            .try_send(Envelope {
                request_id: 0,
                payload: Some(Payload::LayoutUpdate(update)),
            })
            .map_err(|error| self.queue_error(error))
    }

    /// Send a best-effort diagnostic event to the server.
    pub async fn send_log_event(
        &self,
        level: impl Into<String>,
        text: impl Into<String>,
    ) -> Result<(), RpcError> {
        if !self.is_connected() {
            return Err(RpcError::Disconnected);
        }
        self.outbound
            .try_send(Envelope {
                request_id: 0,
                payload: Some(Payload::LogEvent(LogEvent {
                    level: level.into(),
                    text: text.into(),
                })),
            })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => RpcError::Overloaded,
                mpsc::error::TrySendError::Closed(_) => RpcError::Disconnected,
            })
    }

    fn queue_error(&self, error: mpsc::error::TrySendError<Envelope>) -> RpcError {
        match error {
            mpsc::error::TrySendError::Full(_) => {
                self.closed.send_replace(true);
                RpcError::Overloaded
            }
            mpsc::error::TrySendError::Closed(_) => RpcError::Disconnected,
        }
    }

    /// Read the broker's effective startup configuration.
    pub async fn get_settings(&self) -> Result<crate::message::Settings, RpcError> {
        let response = self
            .request(Payload::GetSettings(crate::message::GetSettings {}))
            .await?;
        match response.payload {
            Some(Payload::Settings(settings)) => Ok(settings),
            _ => Err(RpcError::UnexpectedResponse),
        }
    }

    /// Check service readiness on a control connection, without creating input state.
    pub async fn ping(&self, text: impl Into<String>) -> Result<Pong, RpcError> {
        let response = self
            .request(Payload::Ping(Ping { text: text.into() }))
            .await?;
        match response.payload {
            Some(Payload::Pong(response)) => Ok(response),
            _ => Err(RpcError::UnexpectedResponse),
        }
    }

    pub async fn process_translated_key(
        &self,
        event: KeyEvent,
    ) -> Result<KeyEventResponse, RpcError> {
        self.ensure_input_session(event.token).await?;
        self.request_key_response(Payload::KeyEvent(event)).await
    }

    pub async fn context_command(
        &self,
        command: crate::message::ContextCommand,
    ) -> Result<KeyEventResponse, RpcError> {
        self.ensure_input_session(command.token).await?;
        self.request_key_response(Payload::ContextCommand(command))
            .await
    }

    async fn request_key_response(&self, payload: Payload) -> Result<KeyEventResponse, RpcError> {
        let response = self.request(payload).await?;
        match response.payload {
            Some(Payload::KeyEventResponse(response)) => Ok(response),
            _ => Err(RpcError::UnexpectedResponse),
        }
    }

    async fn ensure_input_session(
        &self,
        token: Option<crate::message::ContextToken>,
    ) -> Result<(), RpcError> {
        if !self.is_connected() {
            return Err(RpcError::Disconnected);
        }
        let mut opened = self.input_open.lock().await;
        if *opened {
            return Ok(());
        }
        let reply = self
            .request(Payload::OpenInput(crate::message::OpenInput { token }))
            .await?;
        match reply.payload {
            Some(Payload::InputOpened(v)) if v.token == token => {
                *opened = true;
                Ok(())
            }
            _ => Err(RpcError::UnexpectedResponse),
        }
    }

    /// Ask the server to finish its current work and shut down gracefully.
    pub async fn shutdown(&self, reason: impl Into<String>) -> Result<ShutdownResponse, RpcError> {
        let response = self
            .request(Payload::Shutdown(Shutdown {
                reason: reason.into(),
            }))
            .await?;
        match response.payload {
            Some(Payload::ShutdownResponse(response)) => Ok(response),
            _ => Err(RpcError::UnexpectedResponse),
        }
    }
    async fn request(&self, payload: Payload) -> Result<Envelope, RpcError> {
        if !self.is_connected() {
            return Err(RpcError::Disconnected);
        }
        // Validate before queueing: a caller error must not poison a live connection.
        wire::encode(&Envelope {
            request_id: 1,
            payload: Some(payload.clone()),
        })?;
        let id = self
            .next_request_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if id == 0 {
            self.disconnect().await;
            return Err(RpcError::Protocol("request ID exhausted".into()));
        }
        let (tx, rx) = oneshot::channel();
        match &payload {
            Payload::KeyEvent(key) => crate::input_trace!(
                "rpc.tx request={} token={:?} vk={} lp={} up={}",
                id,
                key.token,
                key.virtual_key,
                key.lparam,
                key.key_up
            ),
            Payload::ContextCommand(command) => crate::input_trace!(
                "rpc.context request={} token={:?} action={}",
                id,
                command.token,
                command.action
            ),
            _ => (),
        }
        {
            let mut pending = self.pending.lock().unwrap_or_else(|p| p.into_inner());
            let requests = pending.as_mut().ok_or(RpcError::Disconnected)?;
            if requests.len() >= 64 {
                return Err(RpcError::Overloaded);
            }
            requests.insert(id, tx);
        }
        let _call = PendingCall {
            pending: self.pending.clone(),
            id,
        };
        // Cancelling this future drops the registration, even during queueing.
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            self.outbound
                .send(Envelope {
                    request_id: id,
                    payload: Some(payload),
                })
                .await
                .map_err(|_| RpcError::Disconnected)?;
            let reply = rx.await.map_err(|_| RpcError::Disconnected)??;
            if let Some(Payload::Failure(error)) = &reply.payload {
                return Err(RpcError::Remote {
                    code: crate::message::FailureCode::try_from(error.code)
                        .map_err(|_| RpcError::Protocol("unknown failure code".into()))?,
                    message: error.message.clone(),
                });
            }
            Ok(reply)
        })
        .await
        .map_err(|_| RpcError::Timeout)?
    }
}
