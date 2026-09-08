//! Bounded, text-free flight recorder. No host-thread disk I/O or symbol loading.
use std::{
    cell::RefCell,
    collections::VecDeque,
    panic::Location,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::SystemTime,
};

const CAPACITY: usize = 128;
static WRITERS: AtomicUsize = AtomicUsize::new(0);
static DIALOG_OPEN: AtomicBool = AtomicBool::new(false);
static NEXT_ID: AtomicUsize = AtomicUsize::new(1);
static THREADS: Mutex<Vec<(std::thread::JoinHandle<()>, crate::module::ModuleLease)>> =
    Mutex::new(Vec::new());

pub(crate) fn reap_finished() {
    if let Ok(mut threads) = THREADS.try_lock() {
        threads.retain(|(thread, _)| !thread.is_finished());
    }
}
thread_local! {
    static CURRENT: RefCell<Weak<Recorder>> = RefCell::new(Weak::new());
}

#[derive(Clone)]
struct Event {
    time: SystemTime,
    thread: std::thread::ThreadId,
    site: &'static Location<'static>,
    kind: &'static str,
    value: u64,
}

impl std::fmt::Debug for Event {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} DEBUG {:?} {}:{} {} value={:#x}",
            weasel_common::logging::timestamp(self.time),
            self.thread,
            self.site.file(),
            self.site.line(),
            self.kind,
            self.value
        )
    }
}

struct Recorder {
    id: usize,
    events: Mutex<VecDeque<Event>>,
    first_fault: Mutex<Option<Event>>,
    saved: Arc<AtomicBool>,
}

pub(crate) struct FaultState {
    flag: AtomicBool,
    recorder: Arc<Recorder>,
    window: AtomicUsize,
    report_attempts: AtomicUsize,
}

pub(crate) struct TrackedGuard<'a, T> {
    guard: std::sync::MutexGuard<'a, T>,
    fault: &'a FaultState,
    address: u64,
}

impl<T> std::ops::Deref for TrackedGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.guard
    }
}
impl<T> std::ops::DerefMut for TrackedGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.guard
    }
}
impl<T> Drop for TrackedGuard<'_, T> {
    fn drop(&mut self) {
        self.fault.event("lock.release", self.address);
    }
}

impl FaultState {
    #[track_caller]
    pub(crate) fn track<'a, T>(
        &'a self,
        guard: std::sync::MutexGuard<'a, T>,
        address: u64,
    ) -> TrackedGuard<'a, T> {
        self.event("lock.acquired", address);
        TrackedGuard {
            guard,
            fault: self,
            address,
        }
    }
    pub(crate) fn new(_: bool) -> Self {
        let recorder = Arc::new(Recorder {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            events: Mutex::new(VecDeque::with_capacity(CAPACITY)),
            first_fault: Mutex::new(None),
            saved: Arc::new(AtomicBool::new(false)),
        });
        let _ = CURRENT.try_with(|current| *current.borrow_mut() = Arc::downgrade(&recorder));
        Self {
            flag: AtomicBool::new(false),
            recorder,
            window: AtomicUsize::new(0),
            report_attempts: AtomicUsize::new(0),
        }
    }

    pub(crate) fn load(&self, ordering: Ordering) -> bool {
        self.flag.load(ordering)
    }

    #[track_caller]
    pub(crate) fn event(&self, kind: &'static str, value: u64) {
        if self.load(Ordering::Acquire) {
            return;
        }
        let event = Event {
            time: SystemTime::now(),
            thread: std::thread::current().id(),
            site: Location::caller(),
            kind,
            value,
        };
        if let Ok(mut events) = self.recorder.events.try_lock() {
            if events.len() == CAPACITY {
                events.pop_front();
            }
            events.push_back(event);
        }
    }

    #[track_caller]
    pub(crate) fn mark(&self, reason: &'static str, code: u64) {
        if self.flag.swap(true, Ordering::AcqRel) {
            return;
        }
        let fault = Event {
            time: SystemTime::now(),
            thread: std::thread::current().id(),
            site: Location::caller(),
            kind: reason,
            value: code,
        };
        if let Ok(mut first) = self.recorder.first_fault.try_lock() {
            *first = Some(fault.clone());
        }
        self.retry_report();
        self.request_maintenance();
    }

    pub(crate) fn set_window(&self, hwnd: usize) {
        self.window.store(hwnd, Ordering::Release);
    }
    pub(crate) fn request_maintenance(&self) {
        let hwnd = self.window.load(Ordering::Acquire);
        if hwnd != 0 {
            unsafe {
                let _ = crate::bindings::SetTimer(
                    Some(crate::bindings::HWND(hwnd as *mut _)),
                    crate::update_window::MAINTENANCE_TIMER,
                    250,
                    None,
                );
            }
        }
    }
    pub(crate) fn retry_report(&self) {
        if cfg!(test)
            || !self.load(Ordering::Acquire)
            || self.recorder.saved.load(Ordering::Acquire)
        {
            return;
        }
        if self.report_attempts.fetch_add(1, Ordering::AcqRel) >= 4 {
            return;
        }
        if let Some(first) = self
            .recorder
            .first_fault
            .try_lock()
            .ok()
            .and_then(|v| v.clone())
        {
            self.recorder.save(Some(first), None, None);
        }
        self.request_maintenance();
    }

    #[track_caller]
    pub(crate) fn report_quarantine(&self, context_id: u64, reason: &'static str, code: u64) {
        let cause = Event {
            time: SystemTime::now(),
            thread: std::thread::current().id(),
            site: Location::caller(),
            kind: reason,
            value: code,
        };
        self.event(reason, code);
        if !cfg!(test) {
            self.recorder.save(None, None, Some((context_id, cause)));
        }
    }
}

impl Recorder {
    fn save(&self, fault: Option<Event>, dialog: Option<bool>, quarantine: Option<(u64, Event)>) {
        let Ok(mut threads) = THREADS.try_lock() else {
            return;
        };
        threads.retain(|(thread, _)| !thread.is_finished());
        struct Dialog(bool);
        impl Drop for Dialog {
            fn drop(&mut self) {
                if self.0 {
                    DIALOG_OPEN.store(false, Ordering::Release);
                }
            }
        }
        if dialog.is_some() && DIALOG_OPEN.swap(true, Ordering::AcqRel) {
            return;
        }
        let dialog_guard = Dialog(dialog.is_some());
        // Separate fault record survives even if the ring is temporarily busy.
        let events = self
            .events
            .try_lock()
            .map(|v| v.clone())
            .unwrap_or_default();
        if WRITERS
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < 2).then_some(n + 1)
            })
            .is_err()
        {
            return;
        }
        struct Writer;
        impl Drop for Writer {
            fn drop(&mut self) {
                WRITERS.fetch_sub(1, Ordering::Release);
            }
        }
        let writer = Writer;
        // Keep the lease outside the thread until is_finished(), including its
        // Rust epilogue/TLS destructors. Never join from a host callback.
        let lease = crate::module::ModuleLease::new();
        let id = self.id;
        let report_saved = self.saved.clone();
        // Keep quarantine reports separate from first-fault and manual reports.
        let slot = quarantine.as_ref().map_or_else(
            || id % 4 * 2 + usize::from(dialog.is_some()),
            |(context, _)| 8 + (*context % 32) as usize,
        );
        let fault = fault.or_else(|| self.first_fault.try_lock().ok().and_then(|v| v.clone()));
        let thread = std::thread::Builder::new()
            .name("weasel-tip-diagnostic".into())
            .spawn(move || {
                let _writer = writer;
                let _dialog = dialog_guard;
                let _ = std::panic::catch_unwind(|| {
                    let about = weasel_common::about::information("TIP（当前宿主）");
                    if dialog == Some(false) {
                        weasel_common::about::show(&about);
                        return;
                    }
                    let mut summary = format!("{about}\n\nTIP 实例：{id}\n首次故障：{fault:?}\n\n最近事件（完整记录见文件）：\n");
                    for event in events.iter().rev().take(8).rev() {
                        use std::fmt::Write;
                        let _ = writeln!(summary, "{}:{} {} {:#x}", event.site.file(), event.site.line(), event.kind, event.value);
                    }
                    let mut report = format!(
                        "weasel-tip {} arch={} pid={} instance={}\nfirst_fault={fault:?}\n",
                        env!("CARGO_PKG_VERSION"),
                        std::env::consts::ARCH,
                        std::process::id(),
                        id
                    );
                    if let Some((context, cause)) = &quarantine {
                        use std::fmt::Write;
                        let _ = writeln!(report, "quarantine_context={context}\nquarantine_cause={cause:?}");
                    }
                    for event in events {
                        use std::fmt::Write;
                        let _ = writeln!(
                            report,
                            "{} DEBUG {:?} {}:{} {} value={:#x}",
                            weasel_common::logging::timestamp(event.time),
                            event.thread,
                            event.site.file(),
                            event.site.line(),
                            event.kind,
                            event.value
                        );
                    }
                    let mut roots = Vec::new();
                    if let Some(root) = std::env::var_os("LOCALAPPDATA") {
                        roots.push(std::path::PathBuf::from(root));
                    }
                    roots.push(std::env::temp_dir());
                    let mut saved = None;
                    for root in roots {
                        let dir = root.join("Weasel-RS/Diagnostics");
                        if write_report(
                            &dir,
                            slot,
                            report.as_bytes(),
                        )
                        .is_ok()
                        {
                            saved = Some(dir.join(format!("tip-{}-{slot}.log", std::process::id())));
                            if dialog.is_none() && quarantine.is_none() { report_saved.store(true, Ordering::Release); }
                            break;
                        }
                    }
                    if dialog == Some(true) {
                        summary.push_str(&match saved {
                            Some(path) => format!("\n日志：{}", path.display()),
                            None => "\n日志保存失败（目录可能不可写），可按 Ctrl+C 复制此对话框。".into(),
                        });
                        summary.push_str("\n\nCtrl+C 可复制对话框内容。");
                        weasel_common::about::show(&summary);
                    }
                });
            });
        if let Ok(thread) = thread {
            threads.push((thread, lease));
        }
    }
}

fn write_report(dir: &std::path::Path, id: usize, report: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    std::fs::create_dir_all(dir)?;
    // Separate fault/manual slots: exporting cannot overwrite the first fault.
    let path = dir.join(format!("tip-{}-{}.log", std::process::id(), id));
    let mut file = std::fs::File::create(path)?;
    file.write_all(&report[..report.len().min(64 * 1024)])?;
    drop(file);
    // Only our diagnostic files in this dedicated directory are eligible.
    let mut files: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.starts_with("tip-")
                && name.ends_with(".log")
                && name[4..name.len() - 4]
                    .split('-')
                    .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
        })
        .filter_map(|entry| {
            let meta = entry.metadata().ok()?;
            if !meta.is_file() {
                return None;
            }
            Some((meta.modified().ok()?, entry.path()))
        })
        .collect();
    files.sort_by_key(|(time, _)| *time);
    let excess = files.len().saturating_sub(32);
    for (_, path) in files.into_iter().take(excess) {
        let _ = std::fs::remove_file(path);
    }
    Ok(())
}

pub(crate) fn show_dialog(diagnostics: bool) {
    let _ = CURRENT.try_with(|current| {
        if let Some(recorder) = current.borrow().upgrade() {
            recorder.save(None, Some(diagnostics), None);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quarantine_does_not_mark_or_suppress_instance_fault() {
        let fault = FaultState::new(false);
        fault.report_quarantine(7, "edit.request_failed", 0x80004005);
        assert!(!fault.load(Ordering::Acquire));
        assert!(!fault.recorder.saved.load(Ordering::Acquire));
        assert!(fault.recorder.first_fault.lock().unwrap().is_none());
        assert_eq!(
            fault.recorder.events.lock().unwrap().back().unwrap().kind,
            "edit.request_failed"
        );
        fault.mark("edit.write_uncertain", 1);
        assert_eq!(
            fault
                .recorder
                .first_fault
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .kind,
            "edit.write_uncertain"
        );
    }

    #[test]
    fn injected_faults_stay_in_memory_and_preserve_first_cause() {
        let fault = FaultState::new(false);
        fault.mark("first", 1);
        fault.mark("second", 2);
        assert_eq!(
            fault
                .recorder
                .first_fault
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .kind,
            "first"
        );
        assert_eq!(fault.report_attempts.load(Ordering::Acquire), 0);
        assert!(!fault.recorder.saved.load(Ordering::Acquire));
    }

    #[test]
    fn report_is_bounded_and_rotation_preserves_other_files() {
        let dir = std::env::temp_dir().join(format!(
            "weasel-tip-recorder-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("keep.txt"), "keep").unwrap();
        let data = vec![b'x'; 100_000];
        for id in 0..40 {
            write_report(&dir, id, &data).unwrap();
        }
        let files: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(files.len(), 33);
        assert_eq!(
            std::fs::read_to_string(dir.join("keep.txt")).unwrap(),
            "keep"
        );
        for file in &files {
            if file.path().extension().is_some_and(|ext| ext == "log") {
                assert_eq!(file.metadata().unwrap().len(), 64 * 1024);
            }
        }
        // Remove only the explicitly created test files, not a recursive tree.
        for file in files {
            std::fs::remove_file(file.path()).unwrap();
        }
        std::fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn lock_history_records_acquisition_and_release() {
        let state = FaultState::new(false);
        let mutex = Mutex::new(1);
        {
            let mut guard = state.track(mutex.lock().unwrap(), 123);
            *guard = 2;
        }
        let events = state.recorder.events.lock().unwrap();
        assert_eq!(events[0].kind, "lock.acquired");
        assert_eq!(events[1].kind, "lock.release");
        assert_eq!(events[0].value, events[1].value);
    }

    #[test]
    fn ring_is_bounded_and_frozen_after_fault() {
        let state = FaultState::new(false);
        for n in 0..1000 {
            state.event("edit", n);
        }
        assert_eq!(state.recorder.events.lock().unwrap().len(), CAPACITY);
        state.flag.store(true, Ordering::Release);
        state.event("ignored", 0);
        assert_eq!(
            state.recorder.events.lock().unwrap().back().unwrap().value,
            999
        );
    }
}
