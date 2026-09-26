//! 管理主题 UI 工作线程，并把有界、可合并的快照更新分发给主题后端。

#![allow(unsafe_op_in_unsafe_fn)]
use crate::{
    bindings::Windows::Win32::*,
    state::{Mailbox, Owner},
    theme_api::{ThemeBackend, ThemeFactory, UiMode},
};
use std::{
    sync::{Arc, Mutex, OnceLock, mpsc},
    thread,
    time::{Duration, Instant},
};
use weasel_common::{
    comrt::WinRtApartment,
    message::{RenderSnapshot, RendererEvent},
};

const WM_RENDERER_UPDATE: u32 = WM_APP as u32 + 10;
const WM_RENDERER_QUIT: u32 = WM_APP as u32 + 11;
const WM_RENDERER_THEME: u32 = WM_APP as u32 + 12;

/// 加载首选主题及 ten 回退主题的可用工厂。
///
/// DLL 只从 renderer 所在目录的 `themes` 子目录加载；成功或失败结果均缓存到进程退出。
fn theme_candidates(preferred: &str) -> Vec<&'static dyn ThemeFactory> {
    static ELEVEN: OnceLock<Result<crate::theme_dll::Factory, String>> = OnceLock::new();
    static TEN: OnceLock<Result<crate::theme_dll::Factory, String>> = OnceLock::new();
    static ABC: OnceLock<Result<crate::theme_dll::Factory, String>> = OnceLock::new();
    static VOID: OnceLock<Result<crate::theme_dll::Factory, String>> = OnceLock::new();
    static WASM: OnceLock<Result<crate::theme_dll::Factory, String>> = OnceLock::new();
    let preferred = match preferred {
        "eleven" => "eleven",
        "ten" => "ten",
        "abc" => "abc",
        "void" => "void",
        "wasm" => "wasm",
        _ => "ten",
    };
    std::iter::once(preferred)
        .chain((preferred != "ten").then_some("ten"))
        .filter_map(|name| {
            let slot = match name {
                "eleven" => &ELEVEN,
                "ten" => &TEN,
                "abc" => &ABC,
                "void" => &VOID,
                "wasm" => &WASM,
                _ => unreachable!("candidate names are normalized above"),
            };
            let loaded = slot.get_or_init(|| {
                let executable = std::env::current_exe().map_err(|error| error.to_string())?;
                let directory = executable
                    .parent()
                    .ok_or("renderer has no executable directory")?;
                let path = directory
                    .join("themes")
                    .join(format!("weasel_theme_{name}.dll"));
                crate::theme_dll::Factory::load(name, &path)
            });
            match loaded {
                Ok(factory) => Some(factory as &'static dyn ThemeFactory),
                Err(error) => {
                    crate::notifications::theme_unavailable(name, error);
                    None
                }
            }
        })
        .collect()
}

/// 判断消息是否为发给线程队列的运行时控制消息，而不是窗口过程消息。
fn is_thread_message(message: &MSG, kind: u32) -> bool {
    message.hwnd.0.is_null() && message.message == kind
}

/// 控制 UI 工作线程的命令。
///
/// 渲染快照和断开操作带有来源所有者；邮箱会拒绝过期所有者的更新。`Quit`
/// 关闭邮箱并请求线程退出。
pub enum UiCommand {
    /// 用该所有者的最新快照更新候选窗。
    Render(Owner, RenderSnapshot),
    /// 清除指定所有者当前的候选窗状态。
    Disconnect(Owner),
    /// 丢弃待处理快照并请求 UI 线程退出。
    Quit,
}

/// 可从非 UI 线程发送命令的轻量句柄。
///
/// 邮箱受互斥锁保护；渲染更新会合并为最新待处理快照，并通过线程消息唤醒
/// UI 循环。句柄不拥有工作线程，因此可安全克隆供多个发送方使用。
#[derive(Clone)]
pub struct UiCommandSender {
    /// 线程间共享的有界状态邮箱。
    mailbox: Arc<Mutex<Mailbox>>,
    /// 接收线程消息的 UI 工作线程 ID。
    thread_id: u32,
}

/// 已启动的主题 UI 及其通信端点。
///
/// 丢弃句柄会尝试关闭线程；显式调用 [`UiHandle::close`] 可取得超时或线程
/// 错误。事件接收端和完成通知分别用于处理主题动作及观察工作线程结束状态。
pub struct UiHandle {
    /// 所选主题向运行时声明的能力。
    pub capabilities: crate::theme_api::ThemeCapabilities,
    /// 实际启动的主题名称，可能是回退后选中的主题。
    pub theme: &'static str,
    /// 向该实例的 UI 线程发送命令的共享句柄。
    commands: UiCommandSender,
    /// 主题产生的事件；队列容量固定，拥塞时主题动作可能被丢弃。
    pub events: tokio::sync::mpsc::Receiver<(Owner, RendererEvent)>,
    /// UI 工作线程结束时发送的结果。
    pub finished: tokio::sync::oneshot::Receiver<Result<(), String>>,
    /// 唯一拥有 UI 工作线程 join 句柄的一方。
    thread: Option<thread::JoinHandle<()>>,
}

/// 为主题回调绑定当前所有者，并将事件送回运行时。
#[derive(Clone)]
struct EventSender {
    /// 回调所属的渲染所有者，用于拒绝过期事件。
    pub owner: Owner,
    /// 有界事件队列的发送端。
    sender: tokio::sync::mpsc::Sender<(Owner, RendererEvent)>,
}

/// 在给定期限内等待 UI 线程结束，再回收其 join 句柄。
///
/// 超时不会强制中断线程，因为线程可能仍持有只能在原 apartment 销毁的资源；
/// 调用方必须将此类失败视为当前进程无法安全继续切换后端。
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

/// 区分可尝试下一个主题的初始化失败和必须终止启动的运行时故障。
enum AttemptError {
    /// 当前主题已完成清理，可安全继续回退。
    Failed(String),
    /// 线程可能仍运行或运行时已不可用，继续启动另一后端不安全。
    Fatal(String),
}

/// 按注册顺序尝试主题；普通失败会记录并继续，致命失败立即终止。
fn select_first<T>(
    candidates: &[&'static dyn ThemeFactory],
    mut attempt: impl FnMut(&'static dyn ThemeFactory) -> Result<T, AttemptError>,
) -> Result<T, String> {
    let mut failures = Vec::new();
    for candidate in candidates {
        match attempt(*candidate) {
            Ok(value) => return Ok(value),
            Err(AttemptError::Failed(error)) => {
                crate::notifications::theme_unavailable(candidate.name(), &error);
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

/// 校验 renderer 自己消费的全局字段，并逐项恢复内置默认值。
///
/// broker 只负责合并 JSON；主题专属字段仍交给主题解析器。这里仅处理会在主题创建
/// 前影响所有后端的字段，避免一个错误类型让所有候选主题同时启动失败。
fn normalized_renderer_settings(
    config: &weasel_common::settings::ConfigSnapshot,
) -> weasel_common::settings::ConfigSnapshot {
    let Some(mut root) = config.query(".").ok().flatten().cloned() else {
        return config.clone();
    };
    let defaults: serde_json::Value = serde_json::from_str(include_str!("../../weasel.json"))
        .expect("embedded settings must be valid JSON");
    let Some(root_object) = root.as_object_mut() else {
        return weasel_common::settings::ConfigSnapshot::new(defaults);
    };
    let defaults = defaults
        .as_object()
        .expect("embedded settings root must be an object");
    let mut warnings = Vec::new();
    let checks = [
        (
            "inline_preedit",
            root_object
                .get("inline_preedit")
                .is_some_and(serde_json::Value::is_boolean),
        ),
        (
            "themeSettings",
            root_object
                .get("themeSettings")
                .is_some_and(serde_json::Value::is_object),
        ),
        (
            "input_mode_indicator_duration_ms",
            root_object
                .get("input_mode_indicator_duration_ms")
                .and_then(serde_json::Value::as_u64)
                .is_some_and(|value| (100..=10_000).contains(&value)),
        ),
    ];
    for (name, valid) in checks {
        if !valid {
            root_object.insert(
                name.into(),
                defaults
                    .get(name)
                    .expect("embedded renderer setting must exist")
                    .clone(),
            );
            warnings.push(format!("{name} has an invalid type"));
        }
    }
    if !warnings.is_empty() {
        crate::notifications::invalid_configuration(&warnings.join("; "));
    }
    weasel_common::settings::ConfigSnapshot::new(root)
}

impl UiHandle {
    /// 启动首个可用主题，并在初始化失败时按候选顺序回退。
    ///
    /// 每次尝试最多等待启动握手十秒。只有确认失败线程已完成清理后才会
    /// 尝试下一主题；等待超时视为致命错误，以免两个原生后端并行存活。
    pub fn start(
        theme: &str,
        mode: UiMode,
        config: &weasel_common::settings::ConfigSnapshot,
    ) -> Result<Self, String> {
        let config = normalized_renderer_settings(config);
        let candidates: Vec<_> = theme_candidates(theme).into_iter().collect();
        select_first(&candidates, |registration| {
            Self::start_attempt(registration, mode, &config)
        })
    }

    /// 在独立 UI 线程启动单个主题，并等待其完成初始化握手。
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
            theme: registration.name(),
            capabilities: registration.capabilities(),
            commands: UiCommandSender { mailbox, thread_id },
            events,
            finished,
            thread: Some(worker),
        })
    }

    /// 克隆可跨线程使用的命令发送端。
    pub fn command_sender(&self) -> UiCommandSender {
        self.commands.clone()
    }

    /// 请求线程退出，并最多等待两秒。
    ///
    /// 成功关闭后再次调用是无操作。若线程超时或其完成结果为错误，返回错误；
    /// 超时不会中断仍在运行的 UI 线程。
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
}

impl Drop for UiHandle {
    /// 尽力关闭 UI 线程；析构路径无法向调用方报告关闭错误。
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

    /// 校验并提交命令，必要时向 UI 线程投递一次唤醒消息。
    ///
    /// 多个待处理渲染更新会折叠为最新状态，因此消息数量不随生产速度增长。
    /// 邮箱锁覆盖状态变更和唤醒投递；UI 线程取出状态后会先释放该锁再调用后端。
    /// 无效快照、锁中毒或唤醒失败会返回错误；对过期所有者的命令则安全忽略。
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

    /// 检查给定所有者是否仍是邮箱中的活动所有者。
    pub fn is_owner(&self, owner: Owner) -> bool {
        self.mailbox
            .lock()
            .is_ok_and(|m| !m.closed && m.owner == Some(owner))
    }
}

struct Presentation {
    /// 用于主题通知和诊断的注册名称。
    theme_name: &'static str,
    /// 控制主题是否支持驻留或内嵌预编辑等行为。
    capabilities: crate::theme_api::ThemeCapabilities,
    /// 仅由创建它的 UI 线程调用和销毁的主题后端。
    backend: Box<dyn ThemeBackend>,
    /// 将主题动作绑定到当前所有者后转发给运行时。
    events: EventSender,
    /// 最近一次输入快照；用于内容比较以及外观变化后的重新渲染。
    last: Option<RenderSnapshot>,
    /// 主题事件携带的内容代号；内容改变时递增以使旧回调失效。
    content_id: u64,
    /// 当前模式提示的本地单调截止时间；相同提示 ID 的刷新不会延长它。
    indicator_deadline: Option<(u64, Instant)>,
    /// 新提示从 Renderer 实际收到起应显示的时长。
    indicator_duration: Duration,
}

impl Presentation {
    /// 判断候选视图是否应显示，或因主题支持驻留且输入活动而保留。
    fn should_render(&self, view: &crate::theme_api::CandidateView) -> bool {
        crate::presentation::is_visible(view)
            || (self.capabilities.resident && view.active)
            || (self.capabilities.mode_indicator
                && crate::presentation::is_mode_indicator_visible(view))
    }

    /// 按主题能力、候选优先级和本地截止时间规范化新快照中的提示。
    fn update_indicator(&mut self, snapshot: &mut RenderSnapshot) {
        if !self.capabilities.mode_indicator
            || snapshot.visible
            || !snapshot.items.is_empty()
            || snapshot.preedit.is_some()
        {
            snapshot.mode_indicator = None;
        }
        let Some(indicator) = snapshot.mode_indicator.as_mut() else {
            self.indicator_deadline = None;
            return;
        };
        let now = Instant::now();
        let deadline = match self.indicator_deadline {
            Some((id, deadline)) if id == indicator.id => deadline,
            _ => now + self.indicator_duration,
        };
        if now >= deadline {
            snapshot.mode_indicator = None;
            self.indicator_deadline = None;
            return;
        }
        self.indicator_deadline = Some((indicator.id, deadline));
    }

    /// 返回下一个提示到期时刻，供 UI 消息循环建立一次性计时器。
    fn indicator_deadline(&self) -> Option<Instant> {
        self.indicator_deadline.map(|(_, deadline)| deadline)
    }

    /// 隐藏后端并立即取出、报告它产生的通知。
    fn hide(&mut self) {
        self.backend.hide();
        crate::notifications::drain(self.theme_name, self.backend.as_mut());
    }
    /// 应用某个所有者的新快照；所有者切换会先隐藏旧内容并清空旧快照。
    ///
    /// 内容未变时保留 `content_id`，仅几何变化不使回调身份失效；内容变化时
    /// 递增代号。不可见或无效锚点由可见性策略拦截，后端错误会在通知排空后返回。
    fn apply(&mut self, owner: Owner, snapshot: Option<RenderSnapshot>) -> Result<(), String> {
        if self.events.owner != owner {
            self.hide();
            self.last = None;
            self.indicator_deadline = None;
        }
        self.events.owner = owner;
        match snapshot {
            Some(mut snapshot) => {
                self.update_indicator(&mut snapshot);
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
                if self.should_render(&view) {
                    let events =
                        crate::theme_adapter::events(owner, &snapshot, self.events.sender.clone());
                    {
                        let result = self.backend.render(&view, &events);
                        crate::notifications::drain(self.theme_name, self.backend.as_mut());
                        result?;
                    }
                } else {
                    self.hide();
                }
                self.last = Some(snapshot);
            }
            None => {
                self.hide();
                self.last = None;
                self.indicator_deadline = None;
            }
        }
        Ok(())
    }

    /// 到期时从最后快照移除提示，并把取消后的完整视图交给主题。
    ///
    /// 后端据此只移除提示层；若同时已有候选或驻留 UI，不会被计时器误隐藏。
    fn expire_indicator(&mut self) -> Result<(), String> {
        let Some((id, deadline)) = self.indicator_deadline else {
            return Ok(());
        };
        if Instant::now() < deadline {
            return Ok(());
        }
        self.indicator_deadline = None;
        let Some(snapshot) = self.last.as_mut() else {
            return Ok(());
        };
        if snapshot.mode_indicator.as_ref().map(|value| value.id) != Some(id) {
            return Ok(());
        }
        snapshot.mode_indicator = None;
        self.content_id = self
            .content_id
            .checked_add(1)
            .ok_or("presentation identity exhausted")?;
        let view = crate::theme_adapter::view(snapshot, self.content_id);
        let events =
            crate::theme_adapter::events(self.events.owner, snapshot, self.events.sender.clone());
        let result = self.backend.render(&view, &events);
        crate::notifications::drain(self.theme_name, self.backend.as_mut());
        result
    }

    /// 刷新主题外观，并仅在邮箱所有者仍匹配时重绘最近快照。
    ///
    /// 所有者已切换或断开时，不把旧输入重新显示出来；后端每次调用后都会排空
    /// 通知，即使该调用返回错误也是如此。
    fn refresh(&mut self, current_owner: Option<Owner>) -> Result<(), String> {
        {
            let result = self.backend.refresh_appearance();
            crate::notifications::drain(self.theme_name, self.backend.as_mut());
            result?;
        }
        if current_owner == Some(self.events.owner) {
            if let Some(snapshot) = &self.last {
                let view = crate::theme_adapter::view(snapshot, self.content_id);
                if self.should_render(&view) {
                    let events = crate::theme_adapter::events(
                        self.events.owner,
                        snapshot,
                        self.events.sender.clone(),
                    );
                    {
                        let result = self.backend.render(&view, &events);
                        crate::notifications::drain(self.theme_name, self.backend.as_mut());
                        result?;
                    }
                }
            }
        }
        Ok(())
    }
}

/// 在专属 STA 线程上创建主题并运行 Win32 消息循环。
fn run_ui(
    registration: &'static dyn ThemeFactory,
    mode: UiMode,
    config: weasel_common::settings::ConfigSnapshot,
    mailbox: Arc<Mutex<Mailbox>>,
    events: EventSender,
    ready: &mpsc::SyncSender<Result<u32, String>>,
) -> Result<(), String> {
    unsafe {
        let _apartment = WinRtApartment::initialize_sta().map_err(|e| e.to_string())?;
        let _ = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let config =
            config.with_theme_defaults(registration.name(), registration.default_settings()?)?;
        let inline_preedit = config.required::<bool>(".inline_preedit")?;
        let indicator_duration =
            Duration::from_millis(config.required::<u64>(".input_mode_indicator_duration_ms")?);
        let creation = registration.create(mode, &config);
        for notice in creation.notices {
            crate::notifications::report(registration.name(), notice);
        }
        let backend = creation.backend?;
        if !inline_preedit && !registration.capabilities().preedit {
            crate::diagnostics::record(format_args!(
                "theme {} does not support preedit; using inline preedit",
                registration.name()
            ));
        }
        crate::diagnostics::record(format_args!(
            "theme {} capabilities: {:?}",
            registration.name(),
            registration.capabilities()
        ));
        let mut presentation = Presentation {
            theme_name: registration.name(),
            capabilities: registration.capabilities(),
            backend,
            events,
            last: None,
            content_id: 0,
            indicator_deadline: None,
            indicator_duration,
        };
        // Preview startup includes its first render. Failed initialization or
        // layout leaves the controller's previous preview alive.
        if mode == UiMode::Preview {
            let mut snapshot = crate::preview::synthetic_snapshot();
            if !inline_preedit && registration.capabilities().preedit {
                snapshot.preedit = Some(weasel_common::message::RenderPreedit {
                    text: "nihao".into(),
                    cursor_utf16: 5,
                });
            }
            {
                let mut queue = mailbox.lock().map_err(|_| "renderer mailbox poisoned")?;
                queue.render(crate::preview::PREVIEW_OWNER, snapshot.clone());
                queue.take_pending();
            }
            presentation.apply(crate::preview::PREVIEW_OWNER, Some(snapshot))?;
            let health = presentation.backend.check_health();
            crate::notifications::drain(registration.name(), presentation.backend.as_mut());
            health?;
        }
        let thread_id = GetCurrentThreadId();
        let _appearance =
            crate::appearance::AppearanceSubscription::new(thread_id, WM_RENDERER_THEME);
        ready
            .send(Ok(thread_id))
            .map_err(|_| "renderer startup handshake failed")?;
        let mut message = MSG::default();
        let mut indicator_timer = 0usize;
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
                sync_indicator_timer(&mut presentation, &mut indicator_timer)?;
            }
            if message.message == WM_TIMER as u32
                && message.hwnd.0.is_null()
                && message.wParam.0 == indicator_timer
            {
                indicator_timer = 0;
                presentation.expire_indicator()?;
                sync_indicator_timer(&mut presentation, &mut indicator_timer)?;
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
                && message.message != WM_TIMER as u32
            {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            let health = presentation.backend.check_health();
            crate::notifications::drain(registration.name(), presentation.backend.as_mut());
            health?;
        }
        if indicator_timer != 0 {
            let _ = KillTimer(None, indicator_timer);
        }
        presentation.backend.hide();
        crate::notifications::drain(registration.name(), presentation.backend.as_mut());
        Ok(())
    }
}

/// 依据 Presentation 的单调截止时间维护一个线程级一次性计时器。
unsafe fn sync_indicator_timer(
    presentation: &mut Presentation,
    timer: &mut usize,
) -> Result<(), String> {
    if *timer != 0 {
        let _ = KillTimer(None, *timer);
        *timer = 0;
    }
    let Some(deadline) = presentation.indicator_deadline() else {
        return Ok(());
    };
    let delay = deadline
        .saturating_duration_since(Instant::now())
        .as_millis()
        .clamp(1, u32::MAX as u128) as u32;
    let id = SetTimer(None, 0, delay, None);
    if id == 0 {
        crate::diagnostics::record(format_args!(
            "could not schedule mode indicator timeout; dismissing indicator"
        ));
        presentation.indicator_deadline = presentation
            .indicator_deadline
            .map(|(id, _)| (id, Instant::now()));
        return presentation.expire_indicator();
    }
    *timer = id;
    Ok(())
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
        fn create(
            &self,
            _: UiMode,
            _: &weasel_common::settings::ConfigSnapshot,
        ) -> crate::theme_api::ThemeCreation {
            crate::theme_api::ThemeCreation::from(Err(
                "test factory must not create native resources".into(),
            ))
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
            theme_name: "test",
            capabilities: crate::theme_api::ThemeCapabilities::CANDIDATES_ONLY,
            backend: Box::new(FakeBackend(calls.clone())),
            events: EventSender { owner: 0, sender },
            last: None,
            content_id: 0,
            indicator_deadline: None,
            indicator_duration: Duration::from_millis(800),
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
    fn mode_indicator_keeps_its_deadline_and_yields_to_candidates() {
        let calls = Arc::new(Mutex::new(Calls::default()));
        let (sender, _) = tokio::sync::mpsc::channel(1);
        let mut ui = Presentation {
            theme_name: "test",
            capabilities: crate::theme_api::ThemeCapabilities {
                mode_indicator: true,
                ..crate::theme_api::ThemeCapabilities::CANDIDATES_ONLY
            },
            backend: Box::new(FakeBackend(calls)),
            events: EventSender { owner: 0, sender },
            last: None,
            content_id: 0,
            indicator_deadline: None,
            indicator_duration: Duration::from_millis(800),
        };
        let mut snapshot = RenderSnapshot {
            sequence: 1,
            active: true,
            anchor: Some(weasel_common::message::RenderRect {
                valid: true,
                bottom: 1,
                ..Default::default()
            }),
            mode_indicator: Some(weasel_common::message::ModeIndicator {
                id: 1,
                ascii_mode: true,
                reason: weasel_common::message::ModeIndicatorReason::UserSwitch as i32,
            }),
            ..Default::default()
        };
        ui.apply(1, Some(snapshot.clone())).unwrap();
        let deadline = ui.indicator_deadline().unwrap();

        snapshot.sequence += 1;
        ui.apply(1, Some(snapshot.clone())).unwrap();
        assert_eq!(ui.indicator_deadline(), Some(deadline));

        snapshot.sequence += 1;
        snapshot.visible = true;
        snapshot.items.push(Default::default());
        ui.apply(1, Some(snapshot)).unwrap();
        assert!(ui.indicator_deadline().is_none());
        assert!(ui.last.as_ref().unwrap().mode_indicator.is_none());
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
