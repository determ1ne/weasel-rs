//! 记录 TIP 实例的首个故障，并按需生成隔离的诊断报告。
//!
//! 正常回调和锁访问不写日志；故障标记本身只做有界的内存及原子操作，报告的磁盘 I/O
//! 与对话框显示均放在辅助线程。线程句柄连同 DLL 租约保留到线程完全结束，以免卸载代码。
use std::{
    cell::RefCell,
    panic::Location,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::SystemTime,
};

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
/// 可跨线程格式化的单条故障或隔离事件；调用点由 `track_caller` 捕获。
struct Event {
    /// 事件发生时间及线程，用于关联异步报告中的故障上下文。
    time: SystemTime,
    thread: std::thread::ThreadId,
    /// 编译期调用点和稳定事件类别；不保存用户输入内容。
    site: &'static Location<'static>,
    kind: &'static str,
    /// 可选 HRESULT 或内部数值，以十六进制形式展示。
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
    /// 当前 TIP 实例编号，用于报告区分与槽位选择。
    id: usize,
    /// 仅保留首个实例故障；诊断和隔离事件不会覆盖它。
    first_fault: Mutex<Option<Event>>,
    /// 首故障报告成功写入后的标记，不代表人工导出或隔离报告。
    saved: Arc<AtomicBool>,
}

/// TIP 实例的故障门闩、窗口维护目标及报告重试状态。
///
/// 故障标记为单向状态；实例故障会抑制后续服务操作，独立的隔离报告则不会改变该状态。
pub(crate) struct FaultState {
    /// 实例级单向故障门闩；置位后服务操作应拒绝继续执行。
    flag: AtomicBool,
    /// 本实例首因及报告身份，和窗口线程状态分开保存。
    recorder: Arc<Recorder>,
    /// 接收维护定时器的 HWND 原值，零表示尚未关联窗口。
    window: AtomicUsize,
    /// 故障报告的有界重试次数。
    report_attempts: AtomicUsize,
}

impl FaultState {
    /// 建立实例记录器，并将其弱引用登记到当前线程，供当前实例的“关于/诊断”入口使用。
    pub(crate) fn new(_: bool) -> Self {
        let recorder = Arc::new(Recorder {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
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

    /// 以调用方指定的内存序读取故障门闩。
    pub(crate) fn load(&self, ordering: Ordering) -> bool {
        self.flag.load(ordering)
    }

    /// 只记录第一个故障原因，并触发有界报告重试和 TSF 线程维护通知。
    ///
    /// 此路径不等待锁；锁竞争时仍保留故障门闩，但首因可能无法落入记录器。
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

    /// 更新可接收维护定时器的窗口句柄；零表示当前没有可用窗口。
    pub(crate) fn set_window(&self, hwnd: usize) {
        self.window.store(hwnd, Ordering::Release);
    }
    /// 请求窗口在 TSF 线程稍后执行维护，不在故障发生线程同步调用 TSF。
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
    /// 最多尝试四次异步保存首故障报告；测试构建和已保存实例不会写盘。
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

    /// 记录某个编辑上下文被隔离的原因，不设置实例故障门闩或首故障记录。
    #[track_caller]
    pub(crate) fn report_quarantine(&self, context_id: u64, reason: &'static str, code: u64) {
        let cause = Event {
            time: SystemTime::now(),
            thread: std::thread::current().id(),
            site: Location::caller(),
            kind: reason,
            value: code,
        };
        if !cfg!(test) {
            self.recorder.save(None, None, Some((context_id, cause)));
        }
    }
}

impl Recorder {
    /// 在辅助线程保存报告或显示对话框。
    ///
    /// 线程数、同时写入数和对话框数均受限；所有争用都以跳过本次操作处理，不能阻塞宿主
    /// 回调。线程持有模块租约，句柄由全局列表保留至 `is_finished`，且不会在回调中 join。
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
                    let mut summary = format!("{about}\n\nTIP 实例：{id}\n首次故障：{fault:?}\n");
                    let mut report = format!(
                        "weasel-tip {} arch={} pid={} instance={}\nfirst_fault={fault:?}\n",
                        env!("CARGO_PKG_VERSION"),
                        std::env::consts::ARCH,
                        std::process::id(),
                        id
                    );
                    if let Some((context, cause)) = &quarantine {
                        use std::fmt::Write;
                        let _ = writeln!(
                            report,
                            "quarantine_context={context}\nquarantine_cause={cause:?}"
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
                        if write_report(&dir, slot, report.as_bytes()).is_ok() {
                            saved =
                                Some(dir.join(format!("tip-{}-{slot}.log", std::process::id())));
                            if dialog.is_none() && quarantine.is_none() {
                                report_saved.store(true, Ordering::Release);
                            }
                            break;
                        }
                    }
                    if dialog == Some(true) {
                        summary.push_str(&match saved {
                            Some(path) => format!("\n日志：{}", path.display()),
                            None => "\n日志保存失败（目录可能不可写），可按 Ctrl+C 复制此对话框。"
                                .into(),
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

/// 将长度限制为 64 KiB 的报告写入专用目录，并最多保留 32 个符合命名规则的日志文件。
///
/// 轮转只触及本函数识别的诊断日志；目录或文件操作失败原样返回给异步调用方。
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

/// 请求当前线程关联实例显示“关于”或诊断报告。
///
/// TLS 访问及线程争用失败均静默跳过；具体对话框与磁盘工作由 `Recorder::save` 异步执行。
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
}
