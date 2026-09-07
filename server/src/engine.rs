//! Engine state and sessions are confined to the worker OS thread.
use crate::{
    librime,
    renderer_bridge::{RendererPublisher, render_snapshot},
    session_route,
    worker::Processor,
};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use weasel_common::message::{ContextToken, Failure, FailureCode, InputOpened};
use weasel_common::{
    message::{
        Envelope, KeyEventResponse, RenderRect, RenderSnapshot, RendererEvent, envelope::Payload,
    },
    rpc::RpcConnection,
    runtime_paths::RuntimePaths,
};

pub(crate) const QUEUE_CAPACITY: usize = 128;

pub(crate) enum Work {
    Message {
        client_id: u64,
        connection: Arc<RpcConnection>,
        alive: Arc<AtomicBool>,
        envelope: Envelope,
        _request: weasel_common::rpc::RequestLease,
    },
    Renderer(RendererEvent),
}

struct ClientSession {
    connection_id: u64,
    connection: Arc<RpcConnection>,
    alive: Arc<AtomicBool>,
    session: librime::RimeSession,
    revision: u64,
    route: session_route::SessionRoute,
    anchor: RenderRect,
    last_response: KeyEventResponse,
}

pub(crate) struct Engine {
    // Field order is intentional: destroy every session before dropping the engine.
    clients: HashMap<u64, ClientSession>,
    rime: librime::Librime,
    // Must outlive the Rime owner, including its finalize callback.
    _data_lock: crate::data_lock::DataLock,
    active_client: Option<u64>,
    renderer: RendererPublisher,
    next_session: u64,
}

fn reply(connection: &RpcConnection, request_id: u64, response: KeyEventResponse) {
    if let Err(error) = connection.enqueue(Envelope {
        request_id,
        payload: Some(Payload::KeyEventResponse(response)),
    }) {
        tracing::debug!(%error, "client response enqueue failed");
    }
}

fn failure(connection: &RpcConnection, request_id: u64, code: FailureCode, message: &str) {
    let _ = connection.enqueue(Envelope {
        request_id,
        payload: Some(Payload::Failure(Failure {
            code: code as i32,
            message: message.into(),
        })),
    });
}

fn valid_token(token: Option<&ContextToken>) -> bool {
    token.is_some_and(|token| {
        token.context_id != 0 && token.connection_epoch != 0 && token.generation != 0
    })
}

impl Engine {
    pub fn new(paths: RuntimePaths, renderer: RendererPublisher) -> Result<Self, String> {
        let data_lock = crate::data_lock::DataLock::acquire(&paths.user_data)?;
        crate::init_logging(&paths, "server")?;
        let rime = librime::Librime::load(&paths.executable_directory)?;
        tracing::info!("librime initialized on engine thread");
        Ok(Self {
            clients: HashMap::new(),
            rime,
            _data_lock: data_lock,
            active_client: None,
            renderer,
            next_session: 1,
        })
    }

    fn message(
        &mut self,
        client_id: u64,
        connection: Arc<RpcConnection>,
        alive: Arc<AtomicBool>,
        envelope: Envelope,
    ) {
        if !alive.load(Ordering::Acquire) {
            return;
        }
        let connection_id = client_id;
        let context = match envelope.payload.as_ref() {
            Some(Payload::OpenInput(v)) => v.token.as_ref(),
            Some(Payload::KeyEvent(v)) => v.token.as_ref(),
            Some(Payload::ContextCommand(v)) => v.token.as_ref(),
            _ => None,
        };
        if !valid_token(context) {
            failure(
                &connection,
                envelope.request_id,
                FailureCode::InvalidArgument,
                "input token required",
            );
            return;
        }
        let context_id = context.unwrap().context_id;
        let existing = self.clients.iter().find_map(|(id, client)| {
            (client.connection_id == connection_id
                && client
                    .route
                    .token
                    .as_ref()
                    .is_some_and(|t| t.context_id == context_id))
            .then_some(*id)
        });
        let client_id = match existing {
            Some(id) => id,
            None if matches!(envelope.payload, Some(Payload::OpenInput(_))) => {
                if self
                    .clients
                    .values()
                    .filter(|c| c.connection_id == connection_id)
                    .count()
                    >= 32
                {
                    failure(
                        &connection,
                        envelope.request_id,
                        FailureCode::InvalidArgument,
                        "too many input contexts",
                    );
                    return;
                }
                let id = self.next_session;
                self.next_session = self
                    .next_session
                    .checked_add(1)
                    .expect("session identity exhausted");
                id
            }
            None => {
                failure(
                    &connection,
                    envelope.request_id,
                    FailureCode::InvalidArgument,
                    "OpenInput required before input",
                );
                return;
            }
        };
        if let Some(Payload::ContextCommand(command)) = envelope.payload.as_ref()
            && command.action == weasel_common::message::ContextAction::Destroy as i32
        {
            if let Some(client) = self.clients.get_mut(&client_id)
                && client.route.observe(command.token.as_ref())
            {
                let response = KeyEventResponse {
                    token: command.token,
                    revision: client.revision + 1,
                    ..Default::default()
                };
                reply(&connection, envelope.request_id, response);
                if self.active_client == Some(client_id) {
                    self.renderer.publish(RenderSnapshot {
                        session_id: client_id,
                        token: command.token,
                        revision: client.revision + 1,
                        ..Default::default()
                    });
                    self.active_client = None;
                }
                self.clients.remove(&client_id);
                connection.forget_layout(context_id);
                connection.set_input_state(
                    self.clients
                        .values()
                        .any(|c| c.connection_id == connection_id && c.route.focused),
                    self.clients
                        .values()
                        .any(|c| c.connection_id == connection_id && c.last_response.composing),
                );
            } else {
                failure(
                    &connection,
                    envelope.request_id,
                    FailureCode::StaleContext,
                    "stale destroy context",
                );
            }
            return;
        }
        // Only explicit OpenInput allocates a native session. Reopening is idempotent
        // for the same token; context changes must use ContextCommand.
        if let Some(Payload::OpenInput(open)) = envelope.payload.as_ref() {
            if !valid_token(open.token.as_ref()) {
                failure(
                    &connection,
                    envelope.request_id,
                    FailureCode::InvalidArgument,
                    "nonzero input token required",
                );
                return;
            }
            if let Some(client) = self.clients.get(&client_id) {
                if client.route.token != open.token {
                    failure(
                        &connection,
                        envelope.request_id,
                        FailureCode::InvalidArgument,
                        "input already opened; use context commands",
                    );
                    return;
                }
            } else {
                let session = match self.rime.new_session() {
                    Ok(session) => session,
                    Err(error) => {
                        tracing::error!(client_id, %error, "session creation failed");
                        failure(
                            &connection,
                            envelope.request_id,
                            FailureCode::Internal,
                            "session creation failed",
                        );
                        return;
                    }
                };
                self.clients.insert(
                    client_id,
                    ClientSession {
                        connection_id,
                        connection: connection.clone(),
                        alive,
                        session,
                        revision: 0,
                        route: session_route::SessionRoute {
                            token: open.token.clone(),
                            focused: false,
                        },
                        anchor: Default::default(),
                        last_response: Default::default(),
                    },
                );
            }
            let _ = connection.enqueue(Envelope {
                request_id: envelope.request_id,
                payload: Some(Payload::InputOpened(InputOpened {
                    token: open.token.clone(),
                })),
            });
            return;
        }
        if !self.clients.contains_key(&client_id) {
            if matches!(
                envelope.payload,
                Some(Payload::KeyEvent(_) | Payload::ContextCommand(_))
            ) {
                failure(
                    &connection,
                    envelope.request_id,
                    FailureCode::InvalidArgument,
                    "OpenInput required before input",
                );
            }
            return;
        }
        let token = match envelope.payload.as_ref() {
            Some(Payload::KeyEvent(key)) => {
                if key.test || key.keycode.is_none() {
                    failure(
                        &connection,
                        envelope.request_id,
                        FailureCode::InvalidArgument,
                        "translated non-test key required",
                    );
                    return;
                }
                Some(key.token.as_ref())
            }
            Some(Payload::ContextCommand(command)) => Some(command.token.as_ref()),
            _ => None,
        };
        if token.is_some_and(|token| !valid_token(token)) {
            failure(
                &connection,
                envelope.request_id,
                FailureCode::InvalidArgument,
                "nonzero input token required",
            );
            return;
        }
        match envelope.payload {
            Some(Payload::KeyEvent(key_event)) => {
                let started = std::time::Instant::now();
                weasel_common::input_trace!(
                    "engine.begin client={} request={} token={:?} vk={} lp={} up={}",
                    client_id,
                    envelope.request_id,
                    key_event.token,
                    key_event.virtual_key,
                    key_event.lparam,
                    key_event.key_up
                );
                let (response, snapshot) = {
                    let client = self
                        .clients
                        .get_mut(&client_id)
                        .expect("client session exists");
                    if !client.route.observe(key_event.token.as_ref()) {
                        failure(
                            &connection,
                            envelope.request_id,
                            FailureCode::StaleContext,
                            "stale or mismatched key context",
                        );
                        return;
                    } else {
                        let focus_changed = self.active_client != Some(client_id);
                        self.active_client = Some(client_id);
                        client.route.focused = true;
                        let mut response = client.session.process_key(&key_event);
                        if response.state_updated {
                            client.revision = client.revision.wrapping_add(1);
                        }
                        response.token = client.route.token.clone();
                        response.revision = client.revision;
                        if response.state_updated {
                            client.last_response = response.clone();
                        }
                        client.last_response.token = client.route.token.clone();
                        let snapshot = (response.state_updated || focus_changed).then(|| {
                            render_snapshot(
                                client_id,
                                client.revision,
                                &client.last_response,
                                &client.anchor,
                            )
                        });
                        (response, snapshot)
                    }
                };
                weasel_common::input_trace!(
                    "engine.end client={} request={} token={:?} revision={} eaten={} state={} preedit_bytes={} commit_bytes={} elapsed_us={}",
                    client_id,
                    envelope.request_id,
                    response.token,
                    response.revision,
                    response.eaten,
                    response.state_updated,
                    response.composition.len(),
                    response.commit_text.len(),
                    started.elapsed().as_micros()
                );
                reply(&connection, envelope.request_id, response);
                if let Some(snapshot) = snapshot {
                    self.renderer.publish(snapshot);
                }
            }
            Some(Payload::ContextCommand(command)) => {
                use weasel_common::message::ContextAction;
                let (response, snapshot) = {
                    let client = self
                        .clients
                        .get_mut(&client_id)
                        .expect("client session exists");
                    let action = ContextAction::try_from(command.action)
                        .unwrap_or(ContextAction::Unspecified);
                    if action == ContextAction::Unspecified {
                        failure(
                            &connection,
                            envelope.request_id,
                            FailureCode::InvalidArgument,
                            "unknown context action",
                        );
                        return;
                    }
                    let previous_context =
                        client.route.token.as_ref().map(|token| token.context_id);
                    if !client.route.observe_command(command.token.as_ref(), action) {
                        failure(
                            &connection,
                            envelope.request_id,
                            FailureCode::StaleContext,
                            "stale or mismatched command context",
                        );
                        return;
                    } else {
                        if previous_context
                            != client.route.token.as_ref().map(|token| token.context_id)
                        {
                            client.session.context_action(ContextAction::Cancel);
                            client.last_response = Default::default();
                            client.anchor = Default::default();
                        }
                        let was_active = self.active_client == Some(client_id);
                        match action {
                            ContextAction::Focus => {
                                client.route.focused = true;
                                self.active_client = Some(client_id);
                            }
                            ContextAction::Blur | ContextAction::HostTerminated => {
                                client.route.focused = false;
                                if was_active {
                                    self.active_client = None;
                                }
                            }
                            _ => {}
                        }
                        let mut response = client.session.context_action(action);
                        client.revision = client.revision.wrapping_add(1);
                        response.token = client.route.token.clone();
                        response.revision = client.revision;
                        if response.state_updated {
                            client.last_response = response.clone();
                        }
                        // Focus acknowledgements must not replay the previous commit.
                        client.last_response.token = client.route.token.clone();
                        let snapshot = if self.active_client == Some(client_id) {
                            Some(render_snapshot(
                                client_id,
                                client.revision,
                                &client.last_response,
                                &client.anchor,
                            ))
                        } else if was_active {
                            Some(RenderSnapshot {
                                session_id: client_id,
                                revision: client.revision,
                                token: client.route.token.clone(),
                                ..Default::default()
                            })
                        } else {
                            None
                        };
                        (response, snapshot)
                    }
                };
                reply(&connection, envelope.request_id, response);
                if let Some(snapshot) = snapshot {
                    self.renderer.publish(snapshot);
                }
            }
            _ => (),
        }
    }

    fn renderer_event(&mut self, event: RendererEvent) {
        let Some(client) = self.clients.get_mut(&event.session_id) else {
            tracing::debug!(
                session_id = event.session_id,
                "ignored renderer event for disconnected client"
            );
            return;
        };
        if self.active_client != Some(event.session_id)
            || !client.alive.load(Ordering::Acquire)
            || !client.route.accepts_ui(&event, client.revision)
        {
            return;
        }
        let mut response = client.session.process_renderer_event(&event);
        client.revision = client.revision.wrapping_add(1);
        response.token = client.route.token.clone();
        response.revision = client.revision;
        client.last_response = response.clone();
        let snapshot =
            render_snapshot(event.session_id, client.revision, &response, &client.anchor);
        let connection = Arc::clone(&client.connection);
        reply(&connection, 0, response);
        self.renderer.publish(snapshot);
    }
}

impl Processor<Work> for Engine {
    fn process(&mut self, work: Work) {
        match work {
            Work::Message {
                client_id,
                connection,
                alive,
                envelope,
                _request,
            } => {
                self.message(client_id, connection, alive, envelope);
                // Reclassify input before dropping the request's eviction guard.
                self.idle();
                drop(_request);
            }
            Work::Renderer(event) => self.renderer_event(event),
        }
    }

    fn idle(&mut self) {
        // Poll the active connection's latest geometry, never the input FIFO.
        // Layout arrival explicitly wakes the engine, including the final drag position.
        if let Some(client_id) = self.active_client
            && let Some(client) = self.clients.get_mut(&client_id)
            && client.alive.load(Ordering::Acquire)
            && let Some(update) = client
                .connection
                .take_layout_for(client.route.token.as_ref())
        {
            let anchor = update.anchor.unwrap_or_default();
            if client.anchor != anchor {
                client.anchor = anchor;
                if !client.last_response.candidates.is_empty() {
                    self.renderer.publish(render_snapshot(
                        client_id,
                        client.revision,
                        &client.last_response,
                        &client.anchor,
                    ));
                }
            }
        }
        let hide = self.active_client.and_then(|client_id| {
            self.clients
                .get(&client_id)
                .filter(|client| !client.alive.load(Ordering::Acquire))
                .map(|client| RenderSnapshot {
                    session_id: client_id,
                    revision: client.revision.wrapping_add(1),
                    token: client.route.token.clone(),
                    ..Default::default()
                })
        });
        self.clients
            .retain(|_, client| client.alive.load(Ordering::Acquire));
        let mut connections = HashMap::new();
        for client in self.clients.values() {
            let entry = connections.entry(client.connection_id).or_insert((
                &client.connection,
                false,
                false,
            ));
            entry.1 |= client.route.focused;
            entry.2 |= client.last_response.composing;
        }
        for (connection, focused, composing) in connections.values() {
            connection.set_input_state(*focused, *composing);
        }
        if let Some(client_id) = self.active_client {
            if !self.clients.contains_key(&client_id) {
                self.active_client = None;
                if let Some(hide) = hide {
                    self.renderer.publish(hide);
                }
            }
        }
    }
}
