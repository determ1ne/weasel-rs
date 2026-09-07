//! Named Pipe server primitives.

use tokio::{
    net::windows::named_pipe::{NamedPipeServer, ServerOptions},
    sync::{Mutex, mpsc, oneshot, watch},
};

use crate::message::{
    Envelope, KeyEventResponse, LayoutUpdate, LogEvent, PeerRole, Pong, RenderSnapshot,
    RendererEvent, ShutdownResponse, envelope::Payload,
};

use super::{RpcError, read_frame, wire, write_frame};

fn validate_peer_role(
    local: PeerRole,
    peer: i32,
    allow_unspecified: bool,
) -> Result<PeerRole, RpcError> {
    let peer =
        PeerRole::try_from(peer).map_err(|_| RpcError::Protocol("unknown peer role".into()))?;
    let allowed = match local {
        PeerRole::Server => {
            matches!(peer, PeerRole::Tip | PeerRole::Broker)
                || (allow_unspecified && peer == PeerRole::Unspecified)
        }
        PeerRole::Renderer => matches!(peer, PeerRole::Server | PeerRole::Broker),
        PeerRole::Broker => matches!(peer, PeerRole::Renderer | PeerRole::Server),
        _ => false,
    };
    if allowed {
        Ok(peer)
    } else {
        Err(RpcError::Protocol(
            "peer role is not allowed on this endpoint".into(),
        ))
    }
}

fn validate_business(
    local: PeerRole,
    peer: PeerRole,
    message: Envelope,
) -> Result<Envelope, RpcError> {
    let allowed = match (local, peer, message.payload.as_ref()) {
        (
            PeerRole::Broker,
            PeerRole::Renderer | PeerRole::Server,
            Some(Payload::QueryConfig(_)),
        ) => true,
        (
            PeerRole::Server | PeerRole::Renderer,
            PeerRole::Broker,
            Some(Payload::Ping(_) | Payload::Shutdown(_)),
        ) => true,
        (
            PeerRole::Server,
            PeerRole::Tip,
            Some(
                Payload::Ping(_)
                | Payload::OpenInput(_)
                | Payload::KeyEvent(_)
                | Payload::ContextCommand(_)
                | Payload::LayoutUpdate(_)
                | Payload::LogEvent(_),
            ),
        ) => true,
        // The legacy test role gets the union of Tip/Broker operations, never responses.
        (
            PeerRole::Server,
            PeerRole::Unspecified,
            Some(
                Payload::Ping(_)
                | Payload::Shutdown(_)
                | Payload::OpenInput(_)
                | Payload::KeyEvent(_)
                | Payload::ContextCommand(_)
                | Payload::LayoutUpdate(_)
                | Payload::LogEvent(_),
            ),
        ) => true,
        (
            PeerRole::Renderer,
            PeerRole::Server,
            Some(Payload::Ping(_) | Payload::RenderSnapshot(_)),
        ) => true,
        _ => false,
    };
    if allowed {
        Ok(message)
    } else {
        Err(RpcError::Protocol(
            "message is not allowed for peer role".into(),
        ))
    }
}

/// A Named Pipe listener with a pre-created next instance.
///
/// Every connected client owns one pipe instance. Before returning a
/// connection, `accept` creates the next instance. Cancelling the accept
/// future retains the pending instance, including any connecting client.
pub struct RpcServer {
    pipe_name: String,
    instance_id: u64,
    role: PeerRole,
    allow_unspecified_peer: bool,
    next_instance: Mutex<Option<NamedPipeServer>>,
}

/// A connected client/server RPC stream.
pub struct RpcConnection {
    activity: std::sync::Arc<super::activity::Activity>,
    layout: std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<LayoutUpdate>>>,
    incoming: Mutex<mpsc::Receiver<Result<Envelope, RpcError>>>,
    outgoing: mpsc::Sender<(Envelope, oneshot::Sender<Result<(), RpcError>>)>,
    closed: watch::Sender<bool>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Drop for RpcConnection {
    fn drop(&mut self) {
        self.closed.send_replace(true);
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl RpcServer {
    pub fn new(pipe_name: impl Into<String>) -> Self {
        let mut server = Self::with_role(pipe_name, PeerRole::Server);
        // Compatibility for existing raw/test clients using RpcClient::connect.
        server.allow_unspecified_peer = true;
        server
    }

    /// Declare the listener's protocol role. Roles constrain protocol operations;
    /// they are self-reported, not authentication. Server uses a shared input
    /// ACL for sandboxed TIPs; renderer retains the private logon ACL.
    /// Production endpoints should use this strict constructor; new() additionally
    /// accepts Unspecified peers for compatibility with existing test clients.
    pub fn with_role(pipe_name: impl Into<String>, role: PeerRole) -> Self {
        Self {
            pipe_name: pipe_name.into(),
            role,
            allow_unspecified_peer: false,
            instance_id: (std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64
                ^ u64::from(std::process::id()))
            .max(1),
            next_instance: Mutex::new(None),
        }
    }

    pub fn pipe_name(&self) -> &str {
        &self.pipe_name
    }

    fn create_instance(&self, first: bool) -> Result<NamedPipeServer, RpcError> {
        let identity = crate::platform::RuntimeIdentity::current()?;
        let descriptor = if self.role == PeerRole::Server {
            crate::platform::LocalSecurityDescriptor::for_input_pipe(&identity)?
        } else {
            crate::platform::LocalSecurityDescriptor::for_named_pipe(&identity)?
        };
        let mut attributes = descriptor.security_attributes();
        let mut options = ServerOptions::new();
        options
            .reject_remote_clients(true)
            .first_pipe_instance(first);
        // SAFETY: attributes has the SECURITY_ATTRIBUTES ABI and references the
        // owned descriptor, both alive through this synchronous creation call.
        // Windows copies the descriptor; neither pointer survives the call.
        let pipe = unsafe {
            options.create_with_security_attributes_raw(
                &self.pipe_name,
                (&mut attributes as *mut crate::platform::SecurityAttributes).cast(),
            )?
        };
        Ok(pipe)
    }

    pub async fn accept(&self) -> Result<RpcConnection, RpcError> {
        // Keep the instance in listener state across select! cancellation.
        let mut next_instance = self.next_instance.lock().await;
        if next_instance.is_none() {
            *next_instance = Some(self.create_instance(true)?);
        }
        next_instance.as_ref().unwrap().connect().await?;
        // The connected instance remains alive, so the namespace is continuously
        // held while adding subsequent instances for simultaneous clients.
        let replacement = self.create_instance(false)?;
        let pipe = next_instance.replace(replacement).unwrap();
        drop(next_instance);
        Ok(RpcConnection::from_pipe(
            pipe,
            self.instance_id,
            self.role,
            self.allow_unspecified_peer,
        ))
    }

    /// Bind before starting clients, without waiting for a connection.
    pub async fn bind(&self) -> Result<(), RpcError> {
        let mut next = self.next_instance.lock().await;
        if next.is_none() {
            *next = Some(self.create_instance(true)?);
        }
        Ok(())
    }
}

impl RpcConnection {
    fn from_pipe(
        pipe: NamedPipeServer,
        instance_id: u64,
        role: PeerRole,
        allow_unspecified_peer: bool,
    ) -> Self {
        let (mut reader, mut writer) = tokio::io::split(pipe);
        let (tx, incoming) = mpsc::channel(64);
        let (outgoing, mut rx) =
            mpsc::channel::<(Envelope, oneshot::Sender<Result<(), RpcError>>)>(64);
        let (closed, mut read_closed) = watch::channel(false);
        let mut write_closed = closed.subscribe();
        let reader_signal = closed.clone();
        let layout = std::sync::Arc::new(std::sync::Mutex::new(std::collections::VecDeque::<
            LayoutUpdate,
        >::new()));
        let reader_layout = layout.clone();
        let activity = std::sync::Arc::new(super::activity::Activity::default());
        let reader_activity = activity.clone();
        let reader_task = tokio::spawn(async move {
            tokio::select! {
                _ = read_closed.changed() => {}
                _ = async {
                    let peer_role = match wire::read_hello(&mut reader).await
                        .and_then(|hello| validate_peer_role(role, hello.role, allow_unspecified_peer)) {
                        Ok(peer_role) => peer_role,
                        Err(error) => { let _ = tx.send(Err(error)).await; return; }
                    };
                    loop {
                        let result = match read_frame(&mut reader).await {
                            Ok(Some(bytes)) => wire::decode(&bytes).and_then(wire::unpack)
                                .and_then(|message| validate_business(role, peer_role, message)),
                            Ok(None) => break,
                            Err(error) => Err(error),
                        };
                        let failed = result.is_err();
                        if let Ok(Envelope { payload: Some(Payload::LayoutUpdate(update)), .. }) = &result {
                            {
                                let context_id = update.token.as_ref().map_or(0, |token| token.context_id);
                                let mut latest = reader_layout.lock().unwrap_or_else(|p| p.into_inner());
                                latest.retain(|v| v.token.as_ref().map_or(0, |t| t.context_id) != context_id);
                                if latest.len() >= 32 { latest.pop_front(); }
                                latest.push_back(update.clone());
                            }
                            reader_activity.notify();
                            continue;
                        }
                        if result.is_ok() && !reader_activity.request() { break; }
                        if tx.send(result).await.is_err() || failed { break; }
                    }
                } => {}
            }
            reader_signal.send_replace(true);
            reader_activity.notify();
        });
        let writer_signal = closed.clone();
        let writer_activity = activity.clone();
        let writer_task = tokio::spawn(async move {
            tokio::select! {
                _ = write_closed.changed() => {}
                _ = async {
                    // Send immediately, independently of receiving the peer hello.
                    // accept() remains usable before the client starts its reader.
                    if tokio::time::timeout(std::time::Duration::from_secs(2),
                        wire::write_hello(&mut writer, role, instance_id))
                        .await.unwrap_or(Err(RpcError::Timeout)).is_err() {
                        return;
                    }
                    while let Some((message, ack)) = rx.recv().await {
                        let result = tokio::time::timeout(std::time::Duration::from_secs(2),
                            write_frame(&mut writer, &message)).await.unwrap_or(Err(RpcError::Timeout));
                        let failed = result.is_err();
                        writer_activity.finish_write();
                        let _ = ack.send(result);
                        if failed { break; }
                    }
                } => {}
            }
            writer_signal.send_replace(true);
            writer_activity.notify();
        });
        Self {
            activity,
            layout,
            incoming: Mutex::new(incoming),
            outgoing,
            closed,
            tasks: vec![reader_task, writer_task],
        }
    }

    pub async fn recv(&self) -> Result<Option<Envelope>, RpcError> {
        Ok(self
            .recv_tracked()
            .await?
            .map(|(envelope, _lease)| envelope))
    }

    pub async fn recv_tracked(&self) -> Result<Option<(Envelope, super::RequestLease)>, RpcError> {
        Ok(self
            .incoming
            .lock()
            .await
            .recv()
            .await
            .transpose()?
            .map(|envelope| (envelope, super::RequestLease(self.activity.clone()))))
    }

    pub fn set_notifier(&self, notify: std::sync::Arc<dyn Fn() + Send + Sync>) {
        self.activity.set_notifier(notify);
    }
    pub fn set_idle(&self, idle: bool) {
        self.activity.set_idle(idle);
    }
    pub fn set_input_state(&self, focused: bool, composing: bool) {
        self.activity.set_input_state(focused, composing);
    }
    pub fn reclaim_candidate(&self) -> Option<(u8, std::time::Instant)> {
        self.activity.candidate()
    }
    pub fn reclaim(&self, expected: (u8, std::time::Instant)) -> bool {
        self.activity.reclaim(expected, || {
            self.closed.send_replace(true);
        })
    }
    pub async fn disconnected(&self) {
        let mut closed = self.closed.subscribe();
        let _ = closed.wait_for(|closed| *closed).await;
    }
    pub fn stale_since(&self, age: std::time::Duration) -> Option<std::time::Instant> {
        self.activity.stale_since(age)
    }
    pub fn evict_if_stale(&self, age: std::time::Duration) -> bool {
        self.activity.evict(age, || {
            self.closed.send_replace(true);
        })
    }

    /// Layout events bypass recv()'s ordered input queue. Keep a future token's
    /// geometry until its preceding queued context command has been applied.
    pub fn take_layout_for(
        &self,
        token: Option<&crate::message::ContextToken>,
    ) -> Option<LayoutUpdate> {
        let mut latest = self.layout.lock().unwrap_or_else(|p| p.into_inner());
        let index = latest
            .iter()
            .position(|update| update.token.as_ref() == token)?;
        latest.remove(index)
    }

    pub fn forget_layout(&self, context_id: u64) {
        self.layout
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|update| {
                update
                    .token
                    .as_ref()
                    .is_none_or(|token| token.context_id != context_id)
            });
    }

    /// Queue in wire order without waiting on a slow peer.
    pub fn enqueue(
        &self,
        envelope: Envelope,
    ) -> Result<oneshot::Receiver<Result<(), RpcError>>, RpcError> {
        if *self.closed.borrow() {
            return Err(RpcError::Disconnected);
        }
        let (tx, rx) = oneshot::channel();
        if !self.activity.write() {
            return Err(RpcError::Disconnected);
        }
        self.outgoing.try_send((envelope, tx)).map_err(|error| {
            self.activity.finish_write();
            match error {
                mpsc::error::TrySendError::Full(_) => {
                    self.closed.send_replace(true);
                    RpcError::Overloaded
                }
                mpsc::error::TrySendError::Closed(_) => RpcError::Disconnected,
            }
        })?;
        Ok(rx)
    }

    pub async fn send(&self, envelope: &Envelope) -> Result<(), RpcError> {
        let reply = self.enqueue(envelope.clone())?;
        tokio::time::timeout(std::time::Duration::from_secs(3), reply)
            .await
            .map_err(|_| RpcError::Timeout)?
            .map_err(|_| RpcError::Disconnected)?
    }

    pub async fn send_log_event(
        &self,
        level: impl Into<String>,
        text: impl Into<String>,
    ) -> Result<(), RpcError> {
        self.send(&Envelope {
            request_id: 0,
            payload: Some(Payload::LogEvent(LogEvent {
                level: level.into(),
                text: text.into(),
            })),
        })
        .await
    }

    pub async fn send_pong(
        &self,
        request_id: u64,
        text: impl Into<String>,
    ) -> Result<(), RpcError> {
        self.send(&Envelope {
            request_id,
            payload: Some(Payload::Pong(Pong { text: text.into() })),
        })
        .await
    }

    pub async fn send_key_event_response(
        &self,
        request_id: u64,
        response: KeyEventResponse,
    ) -> Result<(), RpcError> {
        self.send(&Envelope {
            request_id,
            payload: Some(Payload::KeyEventResponse(response)),
        })
        .await
    }

    pub async fn send_shutdown_response(
        &self,
        request_id: u64,
        response: ShutdownResponse,
    ) -> Result<(), RpcError> {
        self.send(&Envelope {
            request_id,
            payload: Some(Payload::ShutdownResponse(response)),
        })
        .await
    }

    pub async fn send_render_snapshot(&self, snapshot: RenderSnapshot) -> Result<(), RpcError> {
        self.send(&Envelope {
            request_id: 0,
            payload: Some(Payload::RenderSnapshot(snapshot)),
        })
        .await
    }

    pub async fn send_renderer_event(&self, event: RendererEvent) -> Result<(), RpcError> {
        self.send(&Envelope {
            request_id: 0,
            payload: Some(Payload::RendererEvent(event)),
        })
        .await
    }

    pub async fn send_layout_update(&self, update: LayoutUpdate) -> Result<(), RpcError> {
        self.send(&Envelope {
            request_id: 0,
            payload: Some(Payload::LayoutUpdate(update)),
        })
        .await
    }
}
