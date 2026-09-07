//! Bounded UI worker lifecycle and toolkit-independent snapshot dispatch.
#![allow(unsafe_op_in_unsafe_fn)]
use crate::{
    backend::theme_candidates,
    bindings::Windows::Win32::*,
    state::{Mailbox, Owner},
    theme_api::{ThemeBackend, ThemeFactory, UiMode},
};
use std::{
    sync::{Arc, Mutex, mpsc},
    thread,
    time::Duration,
};
use weasel_common::message::{RenderSnapshot, RendererEvent};

const WM_RENDERER_UPDATE: u32 = WM_APP as u32 + 10;
const WM_RENDERER_QUIT: u32 = WM_APP as u32 + 11;
const WM_RENDERER_THEME: u32 = WM_APP as u32 + 12;

fn is_thread_message(message: &MSG, kind: u32) -> bool {
    message.hwnd.0.is_null() && message.message == kind
}

pub enum UiCommand {
    Render(Owner, RenderSnapshot),
    Disconnect(Owner),
    Quit,
}

#[derive(Clone)]
pub struct UiCommandSender {
    mailbox: Arc<Mutex<Mailbox>>,
    thread_id: u32,
}

pub struct UiHandle {
    commands: UiCommandSender,
    pub events: tokio::sync::mpsc::Receiver<(Owner, RendererEvent)>,
    pub finished: tokio::sync::oneshot::Receiver<Result<(), String>>,
    thread: Option<thread::JoinHandle<()>>,
}

#[derive(Clone)]
struct EventSender {
    pub owner: Owner,
    sender: tokio::sync::mpsc::Sender<(Owner, RendererEvent)>,
}

fn join_ui_thread(thread: thread::JoinHandle<()>, timeout: Duration) -> Result<(), String> {
    use std::os::windows::io::AsRawHandle;
    let status = unsafe {
        WaitForSingleObject(
            HANDLE(thread.as_raw_handle()),
            timeout.as_millis().min(u32::MAX as u128 - 1) as u32,
        )
    };
    if status != WAIT_OBJECT_0 as u32 {
        return Err("UI shutdown timed out or wait failed; renderer process must exit".into());
    }
    thread.join().map_err(|_| "UI thread panicked".to_owned())
}

enum AttemptError {
    Failed(String),
    Fatal(String),
}

fn select_first<T>(
    candidates: &[&'static dyn ThemeFactory],
    mut attempt: impl FnMut(&'static dyn ThemeFactory) -> Result<T, AttemptError>,
) -> Result<T, String> {
    let mut failures = Vec::new();
    for candidate in candidates {
        match attempt(*candidate) {
            Ok(value) => return Ok(value),
            Err(AttemptError::Failed(error)) => {
                crate::diagnostics::record(format_args!(
                    "theme {} unavailable: {error}",
                    candidate.name()
                ));
                failures.push(format!("{}: {error}", candidate.name()));
            }
            Err(AttemptError::Fatal(error)) => {
                return Err(format!(
                    "theme {} startup aborted: {error}",
                    candidate.name()
                ));
            }
        }
    }
    Err(format!(
        "no renderer theme could initialize: {}",
        failures.join("; ")
    ))
}

impl UiHandle {
    pub fn start(theme: &str, mode: UiMode, config: &weasel_common::settings::ConfigSnapshot) -> Result<Self, String> {
        select_first(&theme_candidates(theme), |registration| {
            Self::start_attempt(registration, mode, config)
        })
    }

    fn start_attempt(
        registration: &'static dyn ThemeFactory,
        mode: UiMode,
        config: &weasel_common::settings::ConfigSnapshot,
    ) -> Result<Self, AttemptError> {
        let config = config.to_owned();
        let mailbox = Arc::new(Mutex::new(Mailbox::default()));
        let receiver = mailbox.clone();
        let (event_sender, events) = tokio::sync::mpsc::channel(32);
        let (finished_sender, finished) = tokio::sync::oneshot::channel();
        let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name(format!("weasel-renderer-{}", registration.name()))
            .spawn(move || {
                // Cleanup/unwind finishes on this apartment BEFORE failure is
                // reported to the selector. No failed XAML state reaches ten.
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_ui(
                        registration,
                        mode,
                        config,
                        receiver,
                        EventSender {
                            owner: 0,
                            sender: event_sender,
                        },
                        &ready_sender,
                    )
                }))
                .unwrap_or_else(|_| Err("UI worker panicked".to_owned()));
                if let Err(error) = &result {
                    let _ = ready_sender.try_send(Err(error.clone()));
                }
                let _ = finished_sender.send(result);
            })
            .map_err(|e| AttemptError::Fatal(format!("could not create UI thread: {e}")))?;
        let thread_id = match ready_receiver.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(id)) => id,
            Ok(Err(error)) => {
                join_ui_thread(worker, Duration::from_secs(2)).map_err(AttemptError::Fatal)?;
                return Err(AttemptError::Failed(error));
            }
            Err(error) => {
                if let Ok(mut mailbox) = mailbox.try_lock() {
                    mailbox.closed = true;
                }
                // Never wait indefinitely or launch a second backend beside a
                // stuck first thread. The standalone renderer exits on error.
                if worker.is_finished() {
                    let _ = worker.join();
                }
                return Err(AttemptError::Fatal(format!(
                    "UI initialization did not complete: {error}"
                )));
            }
        };
        crate::diagnostics::record(format_args!("using renderer theme {}", registration.name()));
        Ok(Self {
            commands: UiCommandSender { mailbox, thread_id },
            events,
            finished,
            thread: Some(worker),
        })
    }

    pub fn command_sender(&self) -> UiCommandSender {
        self.commands.clone()
    }

    pub fn close(&mut self) -> Result<(), String> {
        if let Some(worker) = self.thread.take() {
            let wake = self.commands.send(UiCommand::Quit);
            join_ui_thread(worker, Duration::from_secs(2))?;
            if let Ok(result) = self.finished.try_recv() {
                result?;
            }
            wake?;
        }
        Ok(())
    }

    /// Blocks until the user-driven UI thread ends (e.g. the preview window is
    /// closed). Unlike close(), no quit is forced and no deadline is imposed: the
    /// user controls when the window goes away.
    pub fn wait_for_close(&mut self) -> Result<(), String> {
        if let Some(worker) = self.thread.take() {
            worker.join().map_err(|_| "UI thread panicked".to_owned())?;
            if let Ok(result) = self.finished.try_recv() {
                result?;
            }
        }
        Ok(())
    }
}

impl Drop for UiHandle {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

impl UiCommandSender {
    #[cfg(test)]
    pub(crate) fn without_ui() -> Self {
        Self {
            mailbox: Arc::new(Mutex::new(Mailbox::default())),
            thread_id: 0,
        }
    }

    pub fn send(&self, command: UiCommand) -> Result<(), String> {
        let mut mailbox = self
            .mailbox
            .lock()
            .map_err(|_| "renderer mailbox poisoned")?;
        let message = match command {
            UiCommand::Render(owner, snapshot) => {
                crate::state::validate(&snapshot)?;
                if !mailbox.render(owner, snapshot) {
                    return Ok(());
                }
                WM_RENDERER_UPDATE
            }
            UiCommand::Disconnect(owner) => {
                if !mailbox.disconnect(owner) {
                    return Ok(());
                }
                WM_RENDERER_UPDATE
            }
            UiCommand::Quit => {
                mailbox.closed = true;
                mailbox.pending = None;
                WM_RENDERER_QUIT
            }
        };
        if message == WM_RENDERER_UPDATE && !mailbox.schedule_wake() {
            return Ok(());
        }
        unsafe {
            if !PostThreadMessageW(self.thread_id, message, WPARAM(0), LPARAM(0)).as_bool() {
                mailbox.closed = true;
                return Err("could not wake UI thread".to_owned());
            }
        }
        Ok(())
    }

    pub fn is_owner(&self, owner: Owner) -> bool {
        self.mailbox
            .lock()
            .is_ok_and(|m| !m.closed && m.owner == Some(owner))
    }
}

struct Apartment;
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe {
            RoUninitialize();
        }
    }
}

struct Presentation {
    backend: Box<dyn ThemeBackend>,
    events: EventSender,
    last: Option<RenderSnapshot>,
    content_id: u64,
}

impl Presentation {
    fn apply(&mut self, owner: Owner, snapshot: Option<RenderSnapshot>) -> Result<(), String> {
        if self.events.owner != owner {
            self.backend.hide();
            self.last = None;
        }
        self.events.owner = owner;
        match snapshot {
            Some(snapshot) => {
                if !self
                    .last
                    .as_ref()
                    .is_some_and(|old| crate::state::same_content(old, &snapshot))
                {
                    self.content_id = self
                        .content_id
                        .checked_add(1)
                        .ok_or("presentation identity exhausted")?;
                }
                let view = crate::theme_adapter::view(&snapshot, self.content_id);
                if crate::presentation::is_visible(&view) {
                    let events =
                        crate::theme_adapter::events(owner, &snapshot, self.events.sender.clone());
                    self.backend.render(&view, &events)?;
                } else {
                    self.backend.hide();
                }
                self.last = Some(snapshot);
            }
            None => {
                self.backend.hide();
                self.last = None;
            }
        }
        Ok(())
    }

    fn refresh(&mut self, current_owner: Option<Owner>) -> Result<(), String> {
        self.backend.refresh_appearance()?;
        if current_owner == Some(self.events.owner) {
            if let Some(snapshot) = &self.last {
                let view = crate::theme_adapter::view(snapshot, self.content_id);
                if crate::presentation::is_visible(&view) {
                    let events = crate::theme_adapter::events(
                        self.events.owner,
                        snapshot,
                        self.events.sender.clone(),
                    );
                    self.backend.render(&view, &events)?;
                }
            }
        }
        Ok(())
    }
}

fn run_ui(
    registration: &'static dyn ThemeFactory,
    mode: UiMode,
    config: weasel_common::settings::ConfigSnapshot,
    mailbox: Arc<Mutex<Mailbox>>,
    events: EventSender,
    ready: &mpsc::SyncSender<Result<u32, String>>,
) -> Result<(), String> {
    unsafe {
        RoInitialize(RO_INIT_SINGLETHREADED)
            .ok()
            .map_err(|e| e.to_string())?;
        let _apartment = Apartment;
        let _ = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let backend = registration.create(mode, &config)?;
        crate::diagnostics::record(format_args!(
            "theme {} capabilities: {:?}",
            registration.name(),
            registration.capabilities()
        ));
        let mut presentation = Presentation {
            backend,
            events,
            last: None,
            content_id: 0,
        };
        let thread_id = GetCurrentThreadId();
        let _appearance =
            crate::appearance::AppearanceSubscription::new(thread_id, WM_RENDERER_THEME);
        ready
            .send(Ok(thread_id))
            .map_err(|_| "renderer startup handshake failed")?;
        let mut message = MSG::default();
        loop {
            if mailbox
                .lock()
                .map_err(|_| "renderer mailbox poisoned")?
                .closed
            {
                break;
            }
            let status = GetMessageW(&mut message, None, 0, 0).0;
            if status == -1 {
                return Err("GetMessageW failed".into());
            }
            if status == 0 || is_thread_message(&message, WM_RENDERER_QUIT) {
                break;
            }
            // Release the transport lock before entering any toolkit/COM call.
            let pending = mailbox
                .lock()
                .map_err(|_| "renderer mailbox poisoned")?
                .take_pending();
            if let Some((owner, snapshot)) = pending {
                presentation.apply(owner, snapshot)?;
            }
            if is_thread_message(&message, WM_RENDERER_THEME)
                || [
                    WM_SETTINGCHANGE as u32,
                    WM_THEMECHANGED as u32,
                    WM_SYSCOLORCHANGE as u32,
                ]
                .contains(&message.message)
            {
                let owner = mailbox
                    .lock()
                    .map_err(|_| "renderer mailbox poisoned")?
                    .owner;
                presentation.refresh(owner)?;
            }
            if !is_thread_message(&message, WM_RENDERER_UPDATE)
                && !is_thread_message(&message, WM_RENDERER_THEME)
                && !presentation.backend.pre_translate(&message)?
            {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            presentation.backend.check_health()?;
        }
        presentation.backend.hide();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_window_messages_do_not_collide_with_runtime_wakes() {
        let mut message = MSG {
            message: WM_RENDERER_QUIT,
            ..Default::default()
        };
        assert!(is_thread_message(&message, WM_RENDERER_QUIT));
        message.hwnd = HWND(std::ptr::dangling_mut());
        assert!(!is_thread_message(&message, WM_RENDERER_QUIT));
    }

    struct FakeFactory(&'static str);
    impl ThemeFactory for FakeFactory {
        fn name(&self) -> &'static str {
            self.0
        }
        fn capabilities(&self) -> crate::theme_api::ThemeCapabilities {
            crate::theme_api::ThemeCapabilities::CANDIDATES_ONLY
        }
        fn create(&self, _: UiMode, _: &weasel_common::settings::ConfigSnapshot) -> Result<Box<dyn ThemeBackend>, String> {
            Err("test factory must not create native resources".into())
        }
    }
    fn candidates() -> [&'static dyn ThemeFactory; 2] {
        [&FakeFactory("eleven"), &FakeFactory("ten")]
    }

    #[test]
    fn first_success_stops_registration_and_failure_falls_back() {
        let mut attempted = Vec::new();
        let selected = select_first(&candidates(), |candidate| {
            attempted.push(candidate.name());
            Ok(candidate.name())
        })
        .unwrap();
        assert_eq!(selected, "eleven");
        assert_eq!(attempted, ["eleven"]);

        attempted.clear();
        let selected = select_first(&candidates(), |candidate| {
            attempted.push(candidate.name());
            if candidate.name() == "eleven" {
                Err(AttemptError::Failed("unavailable".into()))
            } else {
                Ok(candidate.name())
            }
        })
        .unwrap();
        assert_eq!(selected, "ten");
        assert_eq!(attempted, ["eleven", "ten"]);
    }

    #[test]
    fn failures_are_aggregated_but_fatal_timeout_stops_fallback() {
        let error = select_first::<()>(&candidates(), |candidate| {
            Err(AttemptError::Failed(format!("{} failed", candidate.name())))
        })
        .unwrap_err();
        assert!(error.contains("eleven failed") && error.contains("ten failed"));
        let mut attempts = 0;
        assert!(
            select_first::<()>(&candidates(), |_| {
                attempts += 1;
                Err(AttemptError::Fatal("initialization timeout".into()))
            })
            .is_err()
        );
        assert_eq!(attempts, 1);
    }

    #[test]
    fn failed_attempt_cleans_up_on_its_thread_before_next_factory() {
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        struct Resource(Arc<std::sync::atomic::AtomicBool>, thread::ThreadId);
        impl Drop for Resource {
            fn drop(&mut self) {
                assert_eq!(self.1, thread::current().id());
                self.0.store(true, std::sync::atomic::Ordering::Release);
            }
        }
        let selected = select_first(&candidates(), |candidate| {
            if candidate.name() == "eleven" {
                let flag = dropped.clone();
                let worker = thread::spawn(move || {
                    let _resource = Resource(flag, thread::current().id());
                });
                join_ui_thread(worker, Duration::from_secs(1)).unwrap();
                Err(AttemptError::Failed("native init failure".into()))
            } else {
                assert!(dropped.load(std::sync::atomic::Ordering::Acquire));
                Ok(candidate.name())
            }
        })
        .unwrap();
        assert_eq!(selected, "ten");
    }

    #[derive(Default)]
    struct Calls {
        rendered: Vec<u64>,
        hidden: usize,
        refreshed: usize,
    }
    struct FakeBackend(Arc<Mutex<Calls>>);
    impl ThemeBackend for FakeBackend {
        fn render(
            &mut self,
            snapshot: &crate::theme_api::CandidateView,
            _: &crate::theme_api::EventSink,
        ) -> Result<(), String> {
            self.0.lock().unwrap().rendered.push(snapshot.content_id);
            Ok(())
        }
        fn hide(&mut self) {
            self.0.lock().unwrap().hidden += 1;
        }
        fn refresh_appearance(&mut self) -> Result<(), String> {
            self.0.lock().unwrap().refreshed += 1;
            Ok(())
        }
    }

    #[test]
    fn presentation_never_shows_invalid_anchor_or_refreshes_old_owner() {
        let calls = Arc::new(Mutex::new(Calls::default()));
        let (sender, _) = tokio::sync::mpsc::channel(1);
        let mut ui = Presentation {
            backend: Box::new(FakeBackend(calls.clone())),
            events: EventSender { owner: 0, sender },
            last: None,
            content_id: 0,
        };
        let mut snapshot = RenderSnapshot {
            sequence: 1,
            visible: true,
            items: vec![Default::default()],
            ..Default::default()
        };
        ui.apply(1, Some(snapshot.clone())).unwrap();
        assert!(calls.lock().unwrap().rendered.is_empty());
        snapshot.anchor = Some(weasel_common::message::RenderRect {
            valid: true,
            ..Default::default()
        });
        ui.apply(1, Some(snapshot)).unwrap();
        ui.refresh(Some(2)).unwrap();
        assert_eq!(calls.lock().unwrap().rendered, [1]);
        ui.refresh(Some(1)).unwrap();
        assert_eq!(calls.lock().unwrap().rendered, [1, 1]);
        ui.apply(1, None).unwrap();
        ui.refresh(Some(1)).unwrap();
        assert_eq!(calls.lock().unwrap().rendered, [1, 1]);
    }

    #[test]
    fn stalled_ui_shutdown_has_a_deadline_without_interrupting_the_thread() {
        let (release, blocked) = mpsc::channel();
        let (completed, done) = mpsc::channel();
        let worker = thread::spawn(move || {
            blocked.recv_timeout(Duration::from_secs(5)).unwrap();
            completed.send(()).unwrap();
        });
        assert!(join_ui_thread(worker, Duration::from_millis(20)).is_err());
        release.send(()).unwrap();
        done.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(join_ui_thread(thread::spawn(|| {}), Duration::from_secs(1)).is_ok());
    }

    #[test]
    fn event_callbacks_keep_their_owner_and_queue_is_bounded() {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(2);
        let old_callback =
            crate::theme_adapter::events(1, &RenderSnapshot::default(), sender.clone());
        let current = crate::theme_adapter::events(2, &RenderSnapshot::default(), sender);
        old_callback.send(crate::theme_api::UiAction::OpenEmojiPanel);
        current.send(crate::theme_api::UiAction::OpenEmojiPanel);
        current.send(crate::theme_api::UiAction::OpenEmojiPanel);
        assert_eq!(receiver.try_recv().unwrap().0, 1);
        assert_eq!(receiver.try_recv().unwrap().0, 2);
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn event_routing_rejects_superseded_and_disconnected_owners() {
        let mailbox = Arc::new(Mutex::new(Mailbox::default()));
        let commands = UiCommandSender {
            mailbox: mailbox.clone(),
            thread_id: 0,
        };
        let snapshot = RenderSnapshot {
            visible: true,
            sequence: 1,
            ..Default::default()
        };
        mailbox.lock().unwrap().render(1, snapshot.clone());
        assert!(commands.is_owner(1));
        mailbox.lock().unwrap().render(2, snapshot);
        assert!(!commands.is_owner(1));
        assert!(commands.is_owner(2));
        mailbox.lock().unwrap().disconnect(2);
        assert!(!commands.is_owner(2));
    }
}
