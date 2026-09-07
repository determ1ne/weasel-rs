//! Dedicated RPC runtime for the in-process TIP.
//!
//! TSF invokes a TIP from the host application's COM thread. The Named Pipe
//! client owns Tokio tasks, so it is kept on a private thread instead of
//! running an async executor from a TSF callback.

use std::{
    os::windows::io::AsRawHandle,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc::{self, Sender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crate::rpc_diagnostics::report;
use weasel_common::message::{
    ContextAction, ContextCommand, KeyEvent, KeyEventResponse, LayoutUpdate, PeerRole,
};
use weasel_common::rpc::{RpcClient, try_default_pipe_name};

static NEXT_CONNECTION_EPOCH: AtomicU64 = AtomicU64::new(1);
const COMMAND_TIMEOUT: Duration = Duration::from_millis(80);
const STOP_TIMEOUT_MS: u32 = 100;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CommandError {
    NotStarted,
    Closed,
    Full,
}
impl CommandError {
    pub fn name(&self) -> &'static str {
        match self {
            Self::NotStarted => "context.worker_absent",
            Self::Closed => "context.queue_closed",
            Self::Full => "context.queue_full",
        }
    }
}

enum RpcCommand {
    Log {
        level: String,
        text: String,
    },
    KeyEvent {
        event: KeyEvent,
        response: Sender<Option<KeyEventResponse>>,
        deadline: Instant,
    },
    LayoutUpdate(LayoutUpdate),
    Context(ContextCommand),
}

#[derive(Default)]
struct PushTarget {
    layout: LayoutMailbox,
    window: AtomicUsize,
    updates: Mutex<Vec<KeyEventResponse>>,
    failed: AtomicBool,
    connection_failed: AtomicBool,
    epoch: AtomicU64,
    cancellation: AtomicU64,
    connection_generation: AtomicU64,
}

struct LayoutMailbox(tokio::sync::watch::Sender<Option<LayoutUpdate>>);
impl Default for LayoutMailbox {
    fn default() -> Self {
        Self(tokio::sync::watch::channel(None).0)
    }
}

impl PushTarget {
    fn current_epoch(&self) -> u64 {
        let generation = self.cancellation.load(Ordering::Acquire);
        let epoch = self.epoch.load(Ordering::Acquire);
        if self.connection_generation.load(Ordering::Acquire) != generation
            || self.cancellation.load(Ordering::Acquire) != generation
        {
            0
        } else {
            epoch
        }
    }

    fn invalidate(&self) {
        self.cancellation.fetch_add(1, Ordering::AcqRel);
        self.epoch.store(0, Ordering::Release);
        self.connection_failed.store(true, Ordering::Release);
        if let Some(mut updates) = crate::boundary::try_teardown(&self.updates) {
            updates.clear();
        }
        self.notify();
    }

    fn notify(&self) {
        let hwnd = self.window.load(Ordering::Acquire);
        if hwnd != 0 {
            unsafe {
                let _ = crate::bindings::PostMessageW(
                    Some(crate::bindings::HWND(hwnd as *mut _)),
                    crate::update_window::UPDATE_MESSAGE,
                    crate::bindings::WPARAM(0),
                    crate::bindings::LPARAM(0),
                );
            }
        }
    }
}

fn enqueue_update(target: &PushTarget, epoch: u64, response: KeyEventResponse) -> bool {
    // A late response may still be broadcast after PendingCall was cancelled.
    // Reject it here and again when draining: invalidation can race this check.
    if epoch == 0 || target.current_epoch() != epoch {
        return true;
    }
    let queued = match target.updates.lock() {
        Ok(mut queue) if queue.len() < 64 => {
            queue.push(response);
            true
        }
        _ => false,
    };
    if !queued {
        return false;
    }
    let hwnd = target.window.load(Ordering::Acquire);
    hwnd == 0
        || unsafe {
            crate::bindings::PostMessageW(
                Some(crate::bindings::HWND(hwnd as *mut _)),
                crate::update_window::UPDATE_MESSAGE,
                crate::bindings::WPARAM(0),
                crate::bindings::LPARAM(0),
            )
            .as_bool()
        }
}

struct Connection {
    client: RpcClient,
    updates: tokio::task::JoinHandle<()>,
}

impl Connection {
    async fn close(mut self) {
        self.updates.abort();
        let _ = (&mut self.updates).await;
        self.client.disconnect().await;
        // Drop also aborts on cancellation of close(). Runtime destruction is
        // the final fallback, and completes before the worker is joined.
    }
}

impl std::ops::Deref for Connection {
    type Target = RpcClient;
    fn deref(&self) -> &RpcClient {
        &self.client
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        // The subscriber owns a client clone, whose broadcast senders keep its
        // own receiver alive. Never rely on RecvError::Closed to end this task.
        self.updates.abort();
    }
}

async fn connect_client(pipe_name: &str, target: &Arc<PushTarget>) -> Option<Connection> {
    let generation = target.cancellation.load(Ordering::Acquire);
    let client = RpcClient::connect_as(pipe_name, PeerRole::Tip)
        .await
        .inspect_err(|error| report("pipe-connect", Some(pipe_name), error))
        .ok()?;
    let mut updates = client.subscribe_key_responses();

    // Treat the handshake as part of connection establishment.  A pipe can
    // be opened just before the server exits, so connect() alone is not
    // enough to consider the server usable.
    client
        .ping("tip activated")
        .await
        .inspect_err(|error| report("handshake-ping", Some(pipe_name), error))
        .ok()?;
    if target.cancellation.load(Ordering::Acquire) != generation {
        report(
            "handshake-cancelled",
            Some(pipe_name),
            "connection was invalidated during handshake",
        );
        client.disconnect().await;
        return None;
    }
    let epoch = NEXT_CONNECTION_EPOCH.fetch_add(1, Ordering::AcqRel);
    if epoch == 0 {
        report("connection-epoch", Some(pipe_name), "epoch exhausted");
        client.disconnect().await;
        return None;
    }
    target.connection_failed.store(false, Ordering::Release);
    target
        .connection_generation
        .store(generation, Ordering::Release);
    target.epoch.store(epoch, Ordering::Release);
    let target = target.clone();
    let push_client = client.clone();
    let updates = tokio::spawn(async move {
        loop {
            let response = match tokio::select! {
                _ = push_client.disconnected() => {
                    if target.current_epoch() == epoch { target.invalidate(); }
                    break;
                }
                response = updates.recv() => response,
            } {
                Ok(response) => response,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    report("server-events", None, "reply subscription overflowed");
                    if target.current_epoch() == epoch {
                        target.failed.store(true, Ordering::Release);
                        target.invalidate();
                    }
                    push_client.disconnect().await;
                    break;
                }
            };
            if !enqueue_update(&target, epoch, response) {
                report(
                    "server-events",
                    None,
                    "could not enqueue reply for the host thread",
                );
                if target.current_epoch() == epoch {
                    target.failed.store(true, Ordering::Release);
                    target.invalidate();
                }
                push_client.disconnect().await;
                break;
            }
        }
    });
    Some(Connection { client, updates })
}

impl RpcCommand {
    fn deadline(&self) -> Instant {
        match self {
            Self::KeyEvent { deadline, .. } => *deadline,
            _ => Instant::now() + COMMAND_TIMEOUT,
        }
    }
}

/// Connection state owned exclusively by the private runtime thread.
struct WorkerState {
    pipe_name: String,
    client: Option<Connection>,
    push: Arc<PushTarget>,
}

impl WorkerState {
    async fn run(
        mut self,
        mut commands: tokio::sync::mpsc::Receiver<RpcCommand>,
        mut stop: tokio::sync::oneshot::Receiver<()>,
    ) {
        let mut layouts = self.push.layout.0.subscribe();
        loop {
            let command = tokio::select! {
                biased;
                _ = &mut stop => break,
                command = commands.recv() => command,
                changed = layouts.changed() => {
                    if changed.is_err() { break; }
                    let latest = layouts.borrow_and_update().clone();
                    let Some(update) = latest else { continue };
                    Some(RpcCommand::LayoutUpdate(update))
                }
            };
            let Some(command) = command else { break };
            let deadline = command.deadline();
            if Instant::now() >= deadline {
                report(
                    "command-expired",
                    Some(&self.pipe_name),
                    "command expired before dispatch",
                );
                continue;
            }
            self.discard_disconnected_client().await;

            // Keep both the handshake and dispatch within the original budget.
            // Await serially so key replies and context commands retain ordering.
            let establishing = self.client.is_none();
            let result = tokio::select! {
                biased;
                _ = &mut stop => break,
                result = tokio::time::timeout_at(deadline.into(), self.dispatch(command)) => result,
            };
            if result.is_err() {
                report(
                    if establishing {
                        "connect-or-handshake-timeout"
                    } else {
                        "request-timeout"
                    },
                    Some(&self.pipe_name),
                    "command deadline exceeded (80ms budget)",
                );
                // Invalidate queued racing replies before English fallback.
                self.disconnect_invalidated_client().await;
            }
        }
        if let Some(current) = self.client.take() {
            current.close().await;
        }
    }

    async fn ensure_connected(&mut self) {
        if self.client.is_none() {
            self.client = connect_client(&self.pipe_name, &self.push).await;
            if self.client.is_none() {
                self.push.connection_failed.store(true, Ordering::Release);
                self.push.notify();
            }
        }
    }

    fn invalidate_connection(&mut self) -> Option<Connection> {
        self.push.invalidate();
        self.client.take()
    }

    async fn disconnect_invalidated_client(&mut self) {
        if let Some(current) = self.invalidate_connection() {
            current.close().await;
        }
    }

    async fn discard_disconnected_client(&mut self) {
        if self
            .client
            .as_ref()
            .is_some_and(|current| !current.is_connected() || self.push.current_epoch() == 0)
        {
            report(
                "connection-invalidated",
                Some(&self.pipe_name),
                "transport closed or epoch cancelled",
            );
            self.disconnect_invalidated_client().await;
        }
    }

    async fn dispatch(&mut self, command: RpcCommand) {
        match command {
            RpcCommand::Log { level, text } => self.handle_log(level, text).await,
            RpcCommand::KeyEvent {
                event,
                response,
                deadline,
            } => {
                let value = self.handle_key_event(event).await;
                if Instant::now() >= deadline || response.send(value).is_err() {
                    self.disconnect_invalidated_client().await;
                }
            }
            RpcCommand::Context(command) => self.handle_context(command).await,
            RpcCommand::LayoutUpdate(update) => self.handle_layout(update).await,
        }
    }

    async fn handle_log(&mut self, level: String, text: String) {
        self.ensure_connected().await;
        let Some(current) = self.client.as_ref() else {
            return;
        };
        if let Err(error) = current.send_log_event(level, text).await {
            report("send-diagnostic", Some(&self.pipe_name), error);
            self.disconnect_invalidated_client().await;
        }
    }

    async fn handle_key_event(&mut self, mut event: KeyEvent) -> Option<KeyEventResponse> {
        self.ensure_connected().await;
        if let Some(token) = event.token.as_mut() {
            token.connection_epoch = self.push.current_epoch();
        }
        let expected = event.token.clone();
        let current = self.client.as_ref()?;
        match current.process_translated_key(event).await {
            Ok(value) if self.push.current_epoch() != 0 && value.token == expected => Some(value),
            Ok(_) => {
                report(
                    "key-response",
                    Some(&self.pipe_name),
                    "response token mismatch or epoch cancelled",
                );
                self.disconnect_invalidated_client().await;
                None
            }
            Err(error) => {
                report("key-request", Some(&self.pipe_name), error);
                self.disconnect_invalidated_client().await;
                None
            }
        }
    }

    async fn handle_context(&mut self, mut command: ContextCommand) {
        let blur_only = matches!(
            ContextAction::try_from(command.action),
            Ok(ContextAction::Blur | ContextAction::Destroy)
        );
        // Focus establishes the session and queries its actual input mode in
        // this private runtime, never synchronously on the host COM thread.
        if !blur_only {
            self.ensure_connected().await;
        }
        if let Some(token) = command.token.as_mut() {
            token.connection_epoch = self.push.current_epoch();
        }
        let Some(current) = self.client.as_ref() else {
            return;
        };
        if let Err(error) = current.context_command(command).await {
            report("context-request", Some(&self.pipe_name), error);
            self.disconnect_invalidated_client().await;
        }
    }

    async fn handle_layout(&mut self, update: LayoutUpdate) {
        if update
            .token
            .as_ref()
            .is_some_and(|token| token.connection_epoch != self.push.current_epoch())
        {
            return;
        }
        // Layout must never reconnect an idle pipe.
        let Some(current) = self.client.as_ref() else {
            return;
        };
        if let Err(error) = current.send_layout_update(update).await {
            report("layout-send", Some(&self.pipe_name), error);
            self.disconnect_invalidated_client().await;
        }
    }
}

fn run_thread(
    state: WorkerState,
    commands: tokio::sync::mpsc::Receiver<RpcCommand>,
    stop: tokio::sync::oneshot::Receiver<()>,
) {
    crate::boundary::cleanup(|| {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                report("runtime-start", Some(&state.pipe_name), error);
                state.push.invalidate();
                return;
            }
        };
        runtime.block_on(state.run(commands, stop));
    });
}

pub struct RpcWorker {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
    commands: Option<Arc<tokio::sync::mpsc::Sender<RpcCommand>>>,
    push: Arc<PushTarget>,
    // Acquired before spawn; released only after join, including thread epilogue
    // and runtime destruction. A lease dropped inside run_thread is too early.
    // On timeout the lease is intentionally retained until process exit.
    module: Option<crate::module::ModuleLease>,
}

impl RpcWorker {
    pub fn start(&mut self) {
        self.start_resolved(try_default_pipe_name());
    }

    fn start_resolved(&mut self, pipe: std::io::Result<String>) {
        match pipe {
            Ok(pipe) => self.start_with_pipe(pipe),
            Err(error) => {
                report("pipe-identity", None, error);
                self.stop();
                self.push.connection_failed.store(true, Ordering::Release);
            }
        }
    }

    fn start_with_pipe(&mut self, pipe_name: String) {
        self.stop();

        let (stop, stop_receiver) = tokio::sync::oneshot::channel();
        let (command_sender, command_receiver) = tokio::sync::mpsc::channel(64);
        // Do not connect during activation; dispatch establishes connections lazily.
        let state = WorkerState {
            pipe_name,
            client: None,
            push: self.push.clone(),
        };
        let module = crate::module::ModuleLease::new();
        let thread = thread::Builder::new()
            .name("weasel-tip-rpc".to_owned())
            .spawn(move || run_thread(state, command_receiver, stop_receiver))
            .inspect_err(|error| {
                report("worker-start", None, error);
                self.push.invalidate();
            })
            .ok();

        self.stop = thread.as_ref().map(|_| stop);
        self.commands = thread.as_ref().map(|_| Arc::new(command_sender));
        self.module = thread.as_ref().map(|_| module);
        self.thread = thread;
    }

    pub fn process_key_event(&self, event: KeyEvent) -> Option<KeyEventResponse> {
        if self.push.failed.load(Ordering::Acquire) {
            return None;
        }
        let Some(commands) = self.commands.as_ref() else {
            return None;
        };
        let (response, receiver) = mpsc::channel();
        if commands
            .try_send(RpcCommand::KeyEvent {
                event,
                response,
                deadline: Instant::now() + COMMAND_TIMEOUT,
            })
            .inspect_err(|error| report("key-queue", None, error))
            .is_err()
        {
            return None;
        }

        match receiver.recv_timeout(Duration::from_millis(100)) {
            Ok(Some(value)) => Some(value),
            result => {
                if let Err(error) = result {
                    report("host-wait", None, error);
                }
                // The runtime can be descheduled past its 80ms timer. The host
                // must invalidate synchronously before passing the key through.
                self.push.invalidate();
                None
            }
        }
    }

    pub fn connection_epoch(&self) -> u64 {
        self.push.current_epoch()
    }

    pub fn connection_failed(&self) -> bool {
        self.push.connection_failed.load(Ordering::Acquire)
            || self.push.failed.load(Ordering::Acquire)
            || (self.push.epoch.load(Ordering::Acquire) != 0 && self.push.current_epoch() == 0)
    }

    pub fn context_command(&self, command: ContextCommand) -> Result<(), CommandError> {
        self.reset_layout();
        let result = match &self.commands {
            None => Err(CommandError::NotStarted),
            Some(sender) => sender
                .try_send(RpcCommand::Context(command))
                .map_err(|e| match e {
                    tokio::sync::mpsc::error::TrySendError::Full(_) => CommandError::Full,
                    tokio::sync::mpsc::error::TrySendError::Closed(_) => CommandError::Closed,
                }),
        };
        if result.is_err() {
            self.push.invalidate();
        }
        result
    }

    pub fn set_update_window(&self, hwnd: usize) {
        self.push.window.store(hwnd, Ordering::Release);
    }

    pub fn retain_context_updates(&self, contexts: &[u64]) {
        if let Ok(mut updates) = self.push.updates.lock() {
            updates.retain(|response| {
                response
                    .token
                    .as_ref()
                    .is_some_and(|token| contexts.contains(&token.context_id))
            });
        }
    }

    pub fn take_context_updates(&self, context_id: u64) -> Vec<KeyEventResponse> {
        let Ok(mut updates) = self.push.updates.lock() else {
            return Vec::new();
        };
        let epoch = self.push.current_epoch();
        let mut selected = Vec::new();
        updates.retain(|response| {
            let Some(token) = response.token.as_ref() else {
                return false;
            };
            if epoch == 0 || token.connection_epoch != epoch {
                return false;
            }
            if token.context_id == context_id {
                selected.push(response.clone());
                false
            } else {
                true
            }
        });
        selected
    }

    #[cfg(test)]
    pub fn take_updates(&self) -> Vec<KeyEventResponse> {
        match self.push.updates.lock() {
            Ok(mut updates) => {
                let epoch = self.push.current_epoch();
                std::mem::take(&mut *updates)
                    .into_iter()
                    .filter(|response| {
                        epoch != 0
                            && response
                                .token
                                .as_ref()
                                .is_some_and(|token| token.connection_epoch == epoch)
                    })
                    .collect()
            }
            Err(_) => Vec::new(),
        }
    }

    pub fn log(&self, level: impl Into<String>, text: impl Into<String>) {
        let Some(commands) = self.commands.as_ref() else {
            return;
        };
        let _ = commands.try_send(RpcCommand::Log {
            level: level.into(),
            text: text.into(),
        });
    }

    pub fn send_layout_update(&self, update: LayoutUpdate) {
        if self.commands.is_none() || self.push.current_epoch() == 0 {
            return;
        }
        self.push.layout.0.send_if_modified(|latest| {
            if latest.as_ref() == Some(&update) {
                return false;
            }
            *latest = Some(update);
            true
        });
    }

    pub fn reset_layout(&self) {
        self.push.layout.0.send_replace(None);
    }

    pub fn stop(&mut self) {
        self.push.window.store(0, Ordering::Release);
        self.commands.take();
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }

        if let Some(thread) = self.thread.take() {
            // Wait for native termination, including TLS destructors. Rust's
            // is_finished() can become true before the native thread exits.
            // Do not pump COM/window messages in this partially torn-down state.
            let result = unsafe {
                crate::bindings::WaitForSingleObject(
                    crate::bindings::HANDLE(thread.as_raw_handle()),
                    STOP_TIMEOUT_MS,
                )
            };
            if result == crate::bindings::WAIT_OBJECT_0 as u32 {
                let _ = thread.join();
            } else {
                // Never terminate the thread or let DllCanUnloadNow permit
                // unloading code that it may still execute. Leaking this tiny
                // lease is deliberate: even a reaper thread would itself need
                // protection through its native thread-exit epilogue.
                if let Some(module) = self.module.take() {
                    std::mem::forget(module);
                }
                drop(thread);
                weasel_common::input_trace!(
                    "rpc.stop detached wait_result={:?} timeout_ms={}; DLL retained until process exit",
                    result,
                    STOP_TIMEOUT_MS
                );
            }
        }
        // A timed-out worker may hold the old queue lock or publish late data.
        // Never wait for that lock, and never reuse its state on restart.
        self.push = Arc::default();
        self.module.take();
    }
}

impl Default for RpcWorker {
    fn default() -> Self {
        Self {
            stop: None,
            thread: None,
            commands: None,
            push: Arc::default(),
            module: None,
        }
    }
}

impl Drop for RpcWorker {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use weasel_common::rpc::RpcServer;

    #[test]
    fn shared_worker_preserves_other_context_replies() {
        let worker = RpcWorker::default();
        worker.push.epoch.store(1, Ordering::Release);
        let a = update(1);
        let mut b = a.clone();
        b.token.as_mut().unwrap().context_id = 2;
        assert!(enqueue_update(&worker.push, 1, a));
        assert!(enqueue_update(&worker.push, 1, b));
        assert_eq!(worker.take_context_updates(1).len(), 1);
        assert_eq!(worker.take_context_updates(2).len(), 1);
        assert!(worker.take_updates().is_empty());
    }

    #[test]
    fn context_command_reports_queue_failure_and_invalidates_connection() {
        let mut worker = RpcWorker::default();
        assert_eq!(
            worker.context_command(ContextCommand::default()),
            Err(CommandError::NotStarted)
        );
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        worker.commands = Some(Arc::new(tx));
        assert_eq!(worker.context_command(ContextCommand::default()), Ok(()));
        worker.push.epoch.store(42, Ordering::Release);
        assert_eq!(
            worker.context_command(ContextCommand::default()),
            Err(CommandError::Full)
        );
        assert_eq!(worker.connection_epoch(), 0);
        drop(rx);
        assert_eq!(
            worker.context_command(ContextCommand::default()),
            Err(CommandError::Closed)
        );
    }

    #[test]
    fn layout_flood_retains_latest_without_using_command_queue() {
        let mut worker = RpcWorker::default();
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        worker.commands = Some(Arc::new(tx));
        worker.push.epoch.store(1, Ordering::Release);
        let mut changes = worker.push.layout.0.subscribe();
        let mut last = LayoutUpdate::default();
        for x in 0..10000 {
            last.anchor = Some(weasel_common::message::RenderRect {
                left: x,
                ..Default::default()
            });
            worker.send_layout_update(last.clone());
        }
        assert!(rx.try_recv().is_err());
        assert_eq!(*changes.borrow_and_update(), Some(last.clone()));
        worker.send_layout_update(last);
        assert!(!changes.has_changed().unwrap());
        worker.reset_layout();
        assert!(changes.borrow_and_update().is_none());
    }

    fn translated_key() -> KeyEvent {
        KeyEvent {
            keycode: Some(b'a' as i32),
            virtual_key: 0x41,
            test: false,
            token: Some(weasel_common::message::ContextToken {
                context_id: 1,
                connection_epoch: 1,
                generation: 1,
            }),
            ..Default::default()
        }
    }

    async fn open_input(connection: &weasel_common::rpc::RpcConnection) {
        use weasel_common::message::{Envelope, InputOpened, envelope::Payload};
        let open = connection.recv().await.unwrap().unwrap();
        let Some(Payload::OpenInput(open_input)) = open.payload else {
            panic!("expected OpenInput")
        };
        let token = open_input.token.as_ref().unwrap();
        assert!(token.context_id != 0 && token.connection_epoch != 0 && token.generation != 0);
        connection
            .send(&Envelope {
                request_id: open.request_id,
                payload: Some(Payload::InputOpened(InputOpened {
                    token: open_input.token,
                })),
            })
            .await
            .unwrap();
    }

    #[test]
    fn identity_failure_stays_in_english_without_starting_a_worker() {
        let mut worker = RpcWorker::default();
        worker.start_resolved(Err(std::io::Error::other("identity unavailable")));
        assert!(worker.thread.is_none());
        assert!(worker.commands.is_none());
        assert!(worker.module.is_none());
        assert_eq!(worker.connection_epoch(), 0);
        assert!(worker.connection_failed());
        assert!(worker.process_key_event(translated_key()).is_none());
    }

    fn update(epoch: u64) -> KeyEventResponse {
        KeyEventResponse {
            token: Some(weasel_common::message::ContextToken {
                context_id: 1,
                connection_epoch: epoch,
                generation: 1,
            }),
            revision: 1,
            ..Default::default()
        }
    }

    #[test]
    fn host_timeout_rejects_late_reply_even_when_runtime_does_not_run() {
        let mut worker = RpcWorker::default();
        let (commands, mut receiver) = tokio::sync::mpsc::channel(1);
        worker.commands = Some(Arc::new(commands));
        worker.push.epoch.store(42, Ordering::Release);
        assert!(worker.process_key_event(translated_key()).is_none());
        assert_eq!(worker.connection_epoch(), 0);
        assert!(worker.connection_failed());
        // The runtime was not polled, so its deadline never fired.
        let RpcCommand::KeyEvent { response, .. } = receiver.try_recv().unwrap() else {
            panic!("expected key")
        };
        assert!(response.send(Some(update(42))).is_err());
        assert!(enqueue_update(&worker.push, 42, update(42)));
        assert!(worker.take_updates().is_empty());
        // A handshake racing cancellation cannot resurrect its old generation.
        worker.push.epoch.store(43, Ordering::Release);
        assert_eq!(worker.connection_epoch(), 0);
    }

    #[test]
    fn reconnect_discards_old_subscription_and_queued_updates() {
        let worker = RpcWorker::default();
        worker.push.epoch.store(10, Ordering::Release);
        assert!(enqueue_update(&worker.push, 10, update(10)));
        worker.push.invalidate();
        worker.push.connection_generation.store(
            worker.push.cancellation.load(Ordering::Acquire),
            Ordering::Release,
        );
        worker.push.epoch.store(11, Ordering::Release);
        assert!(enqueue_update(&worker.push, 10, update(10)));
        assert!(enqueue_update(&worker.push, 11, update(11)));
        let updates = worker.take_updates();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].token.as_ref().unwrap().connection_epoch, 11);
    }

    #[test]
    fn worker_lease_is_owned_until_join_even_after_thread_finishes() {
        let mut worker = RpcWorker::default();
        worker.start_with_pipe(String::new());
        assert!(worker.module.is_some());
        assert!(!crate::module::can_unload());
        worker.commands.take();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !worker.thread.as_ref().unwrap().is_finished() {
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        assert!(worker.module.is_some());
        assert!(!crate::module::can_unload());
        worker.stop();
        assert!(worker.thread.is_none());
        assert!(worker.module.is_none());
    }

    #[tokio::test]
    async fn dropping_connection_releases_the_subscription_client_clone() {
        let name = format!(r"\\.\pipe\weaselrs-tip-subscription-{}", std::process::id());
        let listener = RpcServer::new(name.clone());
        let _ = tokio::time::timeout(Duration::from_millis(5), listener.accept()).await;
        let (finish, finished) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let connection = listener.accept().await.unwrap();
            let ping = connection.recv().await.unwrap().unwrap();
            connection
                .send_pong(ping.request_id, "ready")
                .await
                .unwrap();
            let _ = finished.await;
        });
        let target = Arc::new(PushTarget::default());
        let weak = Arc::downgrade(&target);
        let connection =
            tokio::time::timeout(Duration::from_secs(2), connect_client(&name, &target))
                .await
                .unwrap()
                .unwrap();
        // Ensure the subscriber actually owns its clone before exercising Drop.
        tokio::task::yield_now().await;
        drop(connection);
        drop(target);
        tokio::time::timeout(Duration::from_secs(2), async {
            while weak.upgrade().is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("old subscriber retained its target/client after connection drop");
        let _ = finish.send(());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn cancelled_handshake_never_publishes_backend_availability() {
        let name = format!(
            r"\\.\pipe\weaselrs-tip-handshake-generation-{}",
            std::process::id()
        );
        let listener = RpcServer::new(name.clone());
        let _ = tokio::time::timeout(Duration::from_millis(5), listener.accept()).await;
        let (received, ping_received) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        let (finish, finished) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let connection = listener.accept().await.unwrap();
            let ping = connection.recv().await.unwrap().unwrap();
            received.send(()).unwrap();
            released.await.unwrap();
            connection
                .send_pong(ping.request_id, "ready")
                .await
                .unwrap();
            let _ = finished.await;
        });
        let target = Arc::new(PushTarget::default());
        let connecting_target = target.clone();
        let connecting =
            tokio::spawn(async move { connect_client(&name, &connecting_target).await });
        tokio::time::timeout(Duration::from_secs(2), ping_received)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(target.current_epoch(), 0);
        target.invalidate();
        release.send(()).unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(2), connecting)
                .await
                .unwrap()
                .unwrap()
                .is_none()
        );
        assert_eq!(target.current_epoch(), 0);
        assert!(target.connection_failed.load(Ordering::Acquire));
        let _ = finish.send(());
        server.await.unwrap();
    }

    #[test]
    fn idle_worker_is_unknown_not_a_connection_failure() {
        let mut worker = RpcWorker::default();
        assert_eq!(worker.connection_epoch(), 0);
        assert!(!worker.connection_failed());
        worker.push.connection_failed.store(true, Ordering::Release);
        assert!(worker.connection_failed());
        worker.stop();
        assert!(!worker.connection_failed());
    }

    #[test]
    fn key_deadline_is_not_renewed_when_dequeued() {
        let (response, _receiver) = mpsc::channel();
        let deadline = Instant::now() - COMMAND_TIMEOUT;
        let command = RpcCommand::KeyEvent {
            event: translated_key(),
            response,
            deadline,
        };
        assert_eq!(command.deadline(), deadline);
    }

    #[tokio::test]
    async fn expired_key_is_dropped_without_connecting() {
        let push = Arc::new(PushTarget::default());
        // An unchanged sentinel verifies that the expired command skips even
        // connection cleanup, as well as replying without network work.
        push.epoch.store(42, Ordering::Release);
        let state = WorkerState {
            pipe_name: String::new(),
            client: None,
            push: push.clone(),
        };
        let (sender, commands) = tokio::sync::mpsc::channel(1);
        let (_stop, stop_receiver) = tokio::sync::oneshot::channel();
        let (response, receiver) = mpsc::channel();
        sender
            .try_send(RpcCommand::KeyEvent {
                event: translated_key(),
                response,
                deadline: Instant::now() - COMMAND_TIMEOUT,
            })
            .unwrap();
        drop(sender);
        state.run(commands, stop_receiver).await;
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
        assert_eq!(push.epoch.load(Ordering::Acquire), 42);
    }

    #[tokio::test]
    async fn idle_blur_and_layout_do_not_open_a_connection() {
        let name = format!(r"\\.\pipe\weaselrs-tip-idle-{}", std::process::id());
        let listener = RpcServer::new(name.clone());
        // Keep a listening instance available: an accidental handshake would
        // hang dispatch and fail the timeout below.
        let _ = tokio::time::timeout(Duration::from_millis(5), listener.accept()).await;
        let mut state = WorkerState {
            pipe_name: name,
            client: None,
            push: Arc::default(),
        };
        tokio::time::timeout(Duration::from_secs(1), async {
            for action in [ContextAction::Blur] {
                state
                    .dispatch(RpcCommand::Context(ContextCommand {
                        action: action as i32,
                        ..Default::default()
                    }))
                    .await;
            }
            state
                .dispatch(RpcCommand::LayoutUpdate(Default::default()))
                .await;
        })
        .await
        .unwrap();
        assert!(state.client.is_none());
        assert_eq!(state.push.epoch.load(Ordering::Acquire), 0);
    }

    #[tokio::test]
    async fn focus_queries_real_mode_without_sending_a_key_or_editing_text() {
        use weasel_common::message::envelope::Payload;
        for ascii in [false, true] {
            let name = format!(r"\\.\pipe\weasel-focus-mode-{}-{ascii}", std::process::id());
            let listener = RpcServer::new(&name);
            let _ = tokio::time::timeout(Duration::from_millis(5), listener.accept()).await;
            let server = tokio::spawn(async move {
                let connection = listener.accept().await.unwrap();
                let ping = connection.recv().await.unwrap().unwrap();
                assert!(matches!(ping.payload, Some(Payload::Ping(_))));
                connection
                    .send_pong(ping.request_id, "ready")
                    .await
                    .unwrap();
                open_input(&connection).await;
                let request = connection.recv().await.unwrap().unwrap();
                let Some(Payload::ContextCommand(command)) = request.payload else {
                    panic!("expected Focus, not a key event");
                };
                assert_eq!(command.action, ContextAction::Focus as i32);
                connection
                    .send_key_event_response(
                        request.request_id,
                        KeyEventResponse {
                            token: command.token,
                            revision: 1,
                            ascii_mode: Some(ascii),
                            ..Default::default()
                        },
                    )
                    .await
                    .unwrap();
                connection
            });
            let push = Arc::new(PushTarget::default());
            let mut state = WorkerState {
                pipe_name: name,
                client: None,
                push: push.clone(),
            };
            tokio::time::timeout(Duration::from_secs(2), async {
                state
                    .dispatch(RpcCommand::Context(ContextCommand {
                        token: translated_key().token,
                        action: ContextAction::Focus as i32,
                    }))
                    .await;
                while push.updates.lock().unwrap().is_empty() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            let updates = push.updates.lock().unwrap().clone();
            assert_eq!(updates.len(), 1);
            assert_eq!(updates[0].ascii_mode, Some(ascii));
            assert_eq!(
                updates[0].token.unwrap().connection_epoch,
                push.current_epoch()
            );
            assert!(!updates[0].state_updated);
            assert!(updates[0].commit_text.is_empty());
            state.client.take().unwrap().close().await;
            drop(server.await.unwrap());
        }
    }

    #[tokio::test]
    async fn stalled_focus_is_bounded_and_invalidates_mode_for_english_fallback() {
        let name = format!(r"\\.\pipe\weasel-focus-stall-{}", std::process::id());
        let listener = RpcServer::new(&name);
        let _ = tokio::time::timeout(Duration::from_millis(5), listener.accept()).await;
        let push = Arc::new(PushTarget::default());
        let state = WorkerState {
            pipe_name: name,
            client: None,
            push: push.clone(),
        };
        let (sender, commands) = tokio::sync::mpsc::channel(1);
        let (_stop, stopped) = tokio::sync::oneshot::channel();
        sender
            .try_send(RpcCommand::Context(ContextCommand {
                token: translated_key().token,
                action: ContextAction::Focus as i32,
            }))
            .unwrap();
        drop(sender);
        tokio::time::timeout(Duration::from_secs(1), state.run(commands, stopped))
            .await
            .unwrap();
        assert!(push.connection_failed.load(Ordering::Acquire));
        assert_eq!(push.current_epoch(), 0);
        assert!(push.updates.lock().unwrap().is_empty());
    }

    #[test]
    fn key_timeout_invalidates_connection_before_english_fallback() {
        let name = format!(r"\\.\pipe\weaselrs-key-timeout-{}", std::process::id());
        let server_name = name.clone();
        let (ready, wait_ready) = mpsc::channel();
        let server = thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let listener = RpcServer::new(server_name);
                let _ = tokio::time::timeout(Duration::from_millis(5), listener.accept()).await;
                ready.send(()).unwrap();
                let connection = listener.accept().await.unwrap();
                let ping = connection.recv().await.unwrap().unwrap();
                connection
                    .send_pong(ping.request_id, "ready")
                    .await
                    .unwrap();
                open_input(&connection).await;
                let key = connection.recv().await.unwrap().unwrap();
                let token = match key.payload {
                    Some(weasel_common::message::envelope::Payload::KeyEvent(key)) => key.token,
                    _ => panic!("expected key"),
                };
                assert!(token.as_ref().unwrap().connection_epoch > 0);
                tokio::time::sleep(Duration::from_millis(130)).await;
                let _ = connection
                    .send_key_event_response(
                        key.request_id,
                        KeyEventResponse {
                            token,
                            revision: 1,
                            eaten: true,
                            commit_text: "late".into(),
                            ..Default::default()
                        },
                    )
                    .await;
            });
        });
        wait_ready.recv_timeout(Duration::from_secs(2)).unwrap();
        let mut worker = RpcWorker::default();
        worker.start_with_pipe(name);
        assert!(worker.process_key_event(translated_key()).is_none());
        assert_eq!(worker.connection_epoch(), 0);
        worker.stop();
        server.join().unwrap();
    }

    // Use an isolated pipe; never load a TIP into an application or start Rime.
    fn stalled_server(
        name: String,
    ) -> (
        mpsc::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
        JoinHandle<()>,
    ) {
        let (ready_tx, ready_rx) = mpsc::channel();
        let (received_tx, received_rx) = mpsc::channel();
        let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
        let thread = thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let server = RpcServer::new(name);
                // Poll accept once so the first pipe instance exists.
                let _ = tokio::time::timeout(Duration::from_millis(5), server.accept()).await;
                ready_tx.send(()).unwrap();
                let connection = server.accept().await.unwrap();
                connection.recv().await.unwrap().unwrap();
                received_tx.send(()).unwrap();
                let _ = finish_rx.await;
            });
        });
        ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        (received_rx, finish_tx, thread)
    }

    #[test]
    fn stop_cancels_stalled_handshake() {
        let name = format!(r"\\.\pipe\weaselrs-tip-stop-{}", std::process::id());
        let (received, finish, server) = stalled_server(name.clone());
        let mut worker = RpcWorker::default();
        worker.start_with_pipe(name);
        worker.log("debug", "connect");
        received.recv_timeout(Duration::from_secs(2)).unwrap();
        let started = Instant::now();
        worker.stop();
        assert!(started.elapsed() < Duration::from_secs(1));
        let _ = finish.send(());
        server.join().unwrap();
    }

    #[test]
    fn stop_times_out_without_waiting_for_worker_or_its_queue_lock() {
        let mut worker = RpcWorker::default();
        let old_push = worker.push.clone();
        old_push.window.store(123, Ordering::Release);
        let background_push = old_push.clone();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        worker.module = Some(crate::module::ModuleLease::new());
        worker.thread = Some(thread::spawn(move || {
            let _queue = background_push.updates.lock().unwrap();
            ready_tx.send(()).unwrap();
            let _ = release_rx.recv_timeout(Duration::from_secs(5));
            // Simulate an old worker publishing state after stop/restart.
            background_push.epoch.store(99, Ordering::Release);
            let _ = finished_tx.send(());
        }));
        ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let started = Instant::now();
        worker.stop();
        let elapsed = started.elapsed();
        // Release before asserting, so failures also allow test cleanup.
        let _ = release_tx.send(());
        finished_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(elapsed < Duration::from_secs(1));
        assert!(worker.thread.is_none());
        assert!(worker.module.is_none());
        assert!(!crate::module::can_unload());
        assert_eq!(old_push.window.load(Ordering::Acquire), 0);
        assert!(!Arc::ptr_eq(&worker.push, &old_push));
        assert_eq!(worker.connection_epoch(), 0);
        assert!(worker.take_updates().is_empty());
        worker.stop(); // Idempotent, with no retained thread to wait for.
    }

    #[test]
    fn worker_reconnects_after_server_disconnect() {
        let name = format!(r"\\.\pipe\weaselrs-tip-reconnect-{}", std::process::id());
        let pipe = name.clone();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
        let server = thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                tokio::time::timeout(Duration::from_secs(3), async move {
                    let listener = RpcServer::new(pipe);
                    let _ = tokio::time::timeout(Duration::from_millis(5), listener.accept()).await;
                    ready_tx.send(()).unwrap();
                    // First connection dies during handshake (deployment).
                    let old = listener.accept().await.unwrap();
                    old.recv().await.unwrap().unwrap();
                    drop(old);
                    let fresh = listener.accept().await.unwrap();
                    let ping = fresh.recv().await.unwrap().unwrap();
                    fresh
                        .send_pong(ping.request_id, "new server")
                        .await
                        .unwrap();
                    open_input(&fresh).await;
                    let key = fresh.recv().await.unwrap().unwrap();
                    let Some(weasel_common::message::envelope::Payload::KeyEvent(event)) =
                        key.payload
                    else {
                        panic!("expected key")
                    };
                    fresh
                        .send_key_event_response(
                            key.request_id,
                            KeyEventResponse {
                                eaten: true,
                                token: event.token,
                                revision: 1,
                                ..Default::default()
                            },
                        )
                        .await
                        .unwrap();
                    let _ = finish_rx.await;
                })
                .await
                .unwrap();
            });
        });
        ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let mut worker = RpcWorker::default();
        worker.start_with_pipe(name);
        assert!(worker.process_key_event(translated_key()).is_none());
        assert!(worker.connection_failed());
        assert!(worker.process_key_event(translated_key()).unwrap().eaten);
        assert!(!worker.connection_failed());
        worker.stop();
        let _ = finish_tx.send(());
        server.join().unwrap();
    }

    #[test]
    fn stalled_handshake_does_not_block_key_fallback_or_stop() {
        let name = format!(r"\\.\pipe\weaselrs-tip-timeout-{}", std::process::id());
        let (received, finish, server) = stalled_server(name.clone());
        let mut worker = RpcWorker::default();
        worker.start_with_pipe(name);
        let started = Instant::now();
        assert!(worker.process_key_event(translated_key()).is_none());
        received.recv_timeout(Duration::from_secs(2)).unwrap();
        worker.stop();
        assert!(started.elapsed() < Duration::from_secs(1));
        let _ = finish.send(());
        server.join().unwrap();
    }
}
