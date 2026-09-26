//! server 与 renderer 子进程的所有权、监控和优雅关闭。

use std::{
    os::windows::io::{AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use crate::{
    bindings::*,
    child_process::Child,
    lifecycle::{Operation, RestartBackoff, shutdown_error_result},
};
use weasel_common::{
    logging::{ComponentLogger, Level},
    message::PeerRole,
    process::{RuntimePaths, SingleInstance},
    rpc::{RpcClient, default_pipe_name, default_renderer_pipe_name},
};

/// broker 是否已经进入不可逆的退出阶段。
static STOPPING: AtomicBool = AtomicBool::new(false);
/// 服务监控线程的唤醒事件，用于打断进程等待和退避等待。
static MONITOR_WAKE: OnceLock<OwnedHandle> = OnceLock::new();
/// 进程级共享服务状态。
static BROKER_STATE: OnceLock<Arc<Mutex<BrokerState>>> = OnceLock::new();
/// 服务生命周期诊断所使用的日志器。
static LOGGER: OnceLock<ComponentLogger> = OnceLock::new();

/// 监控器管理的组件种类。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ManagedComponent {
    Server,
    Renderer,
}

impl ManagedComponent {
    /// 返回组件可执行文件名。
    fn executable(self) -> &'static str {
        match self {
            Self::Server => "weasel-server.exe",
            Self::Renderer => "weasel-renderer.exe",
        }
    }

    /// 判断当前操作是否允许监控器接触该组件。
    fn is_monitored(self, operation: Operation) -> bool {
        match self {
            Self::Server => operation.monitors_server(),
            Self::Renderer => operation.monitors_renderer(),
        }
    }
}

/// broker 正常运行时持有的完整服务状态。
struct BrokerState {
    settings: crate::settings_rpc::SettingsStore,
    notifications: crate::notifications::NotificationCenter,
    paths: RuntimePaths,
    server: Option<Child>,
    renderer: Option<Child>,
    server_retry: RestartBackoff,
    renderer_retry: RestartBackoff,
    operation: Operation,
}

/// 后台部署或重启期间从监督器临时借出的服务资源。
///
/// `Deploy` 只取得 server，`Restart` 同时取得 server 和 renderer。操作完成后必须调用
/// [`restore_after_operation`] 归还资源；这样耗时操作无需长期占用共享状态锁。
pub(crate) struct OperationServices {
    pub(crate) settings: crate::settings_rpc::SettingsStore,
    pub(crate) notifications: crate::notifications::NotificationCenter,
    pub(crate) paths: RuntimePaths,
    pub(crate) server: Option<Child>,
    pub(crate) renderer: Option<Child>,
}

/// 初始化监督器的日志器和 Windows 唤醒事件。
pub(crate) fn initialize(logger: ComponentLogger) -> Result<(), std::io::Error> {
    let _ = LOGGER.set(logger);
    if MONITOR_WAKE.get().is_some() {
        return Ok(());
    }
    let event = unsafe { CreateEventW(None, false, false, None) };
    if event.0.is_null() {
        return Err(std::io::Error::last_os_error());
    }
    let owned = unsafe { OwnedHandle::from_raw_handle(event.0) };
    MONITOR_WAKE
        .set(owned)
        .map_err(|_| std::io::Error::other("service supervisor already initialized"))
}

/// 安装初始服务状态，供监控线程、托盘和后台操作共享。
pub(crate) fn install(
    settings: crate::settings_rpc::SettingsStore,
    notifications: crate::notifications::NotificationCenter,
    paths: RuntimePaths,
    server: Child,
    renderer: Child,
) -> Result<(), &'static str> {
    let state = BrokerState {
        settings,
        notifications,
        paths,
        server: Some(server),
        renderer: Some(renderer),
        server_retry: RestartBackoff::new(Instant::now()),
        renderer_retry: RestartBackoff::new(Instant::now()),
        operation: Operation::Idle,
    };
    BROKER_STATE
        .set(Arc::new(Mutex::new(state)))
        .map_err(|_| "broker service state already initialized")
}

/// 通过统一的受管子进程层启动组件，并完成所有权/就绪握手。
pub(crate) fn start_child(
    directory: &std::path::Path,
    executable: &str,
    arguments: &[&str],
) -> Result<Child, std::io::Error> {
    crate::child_process::start(directory, executable, arguments)
}

/// 将一条服务生命周期诊断写入 broker 日志。
pub(crate) fn diagnostic(text: &str) {
    if let Some(logger) = LOGGER.get() {
        logger.record(Level::INFO, "weasel-broker", format_args!("{text}"));
    }
}

/// 返回 broker 是否正在退出。
pub(crate) fn is_stopping() -> bool {
    STOPPING.load(Ordering::Acquire)
}

/// 进入退出阶段并立即唤醒监控线程。
pub(crate) fn request_stop() {
    STOPPING.store(true, Ordering::Release);
    wake_monitor();
}

/// 唤醒等待子进程句柄或退避期限的监控线程。
pub(crate) fn wake_monitor() {
    if let Some(event) = MONITOR_WAKE.get() {
        unsafe {
            let _ = SetEvent(HANDLE(event.as_raw_handle()));
        }
    }
}

/// 生成供诊断对话框显示的服务状态摘要。
pub(crate) fn diagnostic_summary() -> String {
    let Some(state) = BROKER_STATE.get() else {
        return "服务状态尚未初始化。".into();
    };
    let Ok(state) = state.try_lock() else {
        return "服务状态暂时不可读取。".into();
    };
    format!(
        "路径：{:?}\n托管 server PID：{:?}\n托管 renderer PID：{:?}",
        state.paths,
        state.server.as_ref().map(Child::id),
        state.renderer.as_ref().map(Child::id)
    )
}

/// 为后台操作取出所需服务句柄，并暂停监控器对这些句柄的访问。
pub(crate) fn acquire_for_operation(operation: Operation) -> Option<OperationServices> {
    let state = BROKER_STATE.get()?;
    let mut state = state.lock().unwrap_or_else(|error| error.into_inner());
    state.operation = operation;
    let services = OperationServices {
        settings: state.settings.clone(),
        notifications: state.notifications.clone(),
        paths: state.paths.clone(),
        server: state.server.take(),
        renderer: if operation == Operation::Restart {
            state.renderer.take()
        } else {
            None
        },
    };
    drop(state);
    wake_monitor();
    Some(services)
}

/// 归还后台操作借出的服务句柄，并恢复监控状态。
pub(crate) fn restore_after_operation(
    operation: Operation,
    services: OperationServices,
    failed: bool,
) {
    let Some(state) = BROKER_STATE.get() else {
        return;
    };
    let mut state = state.lock().unwrap_or_else(|error| error.into_inner());
    state.server = services.server;
    state.server_retry.started(Instant::now());
    if operation == Operation::Restart {
        state.renderer = services.renderer;
        state.renderer_retry.started(Instant::now());
    }
    state.operation = if operation == Operation::Restart && failed {
        Operation::Failed
    } else {
        Operation::Idle
    };
    drop(state);
    wake_monitor();
}

/// 监控两个受管组件，在进程退出或句柄缺失时按退避策略恢复。
pub(crate) fn monitor_children() {
    let Some(shared) = BROKER_STATE.get().cloned() else {
        return;
    };
    while !is_stopping() {
        monitor_child(&shared, ManagedComponent::Server);
        monitor_child(&shared, ManagedComponent::Renderer);
        let state = shared.lock().expect("broker state mutex poisoned");
        if is_stopping() || state.operation == Operation::Shutdown {
            break;
        }
        // 锁内复制 OS 句柄，随后释放 Rust 子进程所有权锁再等待。
        let mut handles = Vec::new();
        let mut timeout = INFINITE;
        for (component, child, retry) in [
            (ManagedComponent::Server, &state.server, &state.server_retry),
            (
                ManagedComponent::Renderer,
                &state.renderer,
                &state.renderer_retry,
            ),
        ] {
            if !component.is_monitored(state.operation) {
                continue;
            }
            if let Some(child) = child {
                match unsafe { BorrowedHandle::borrow_raw(child.as_raw_handle()) }
                    .try_clone_to_owned()
                {
                    Ok(handle) => handles.push(handle),
                    Err(_) => timeout = timeout.min(1000),
                }
            } else {
                timeout = timeout.min(
                    retry
                        .remaining(Instant::now())
                        .as_millis()
                        .max(1)
                        .min(u32::MAX as u128 - 1) as u32,
                );
            }
        }
        drop(state);
        let mut raw = vec![HANDLE(
            MONITOR_WAKE
                .get()
                .expect("monitor event initialized")
                .as_raw_handle(),
        )];
        raw.extend(handles.iter().map(|handle| HANDLE(handle.as_raw_handle())));
        if unsafe { WaitForMultipleObjects(&raw, false, timeout) } == WAIT_FAILED {
            break;
        }
    }
}

/// 检查并按需恢复一个受管组件。
fn monitor_child(shared: &Arc<Mutex<BrokerState>>, component: ManagedComponent) {
    let mut guard = shared.lock().unwrap_or_else(|error| error.into_inner());
    let state = &mut *guard;
    if is_stopping() || !component.is_monitored(state.operation) {
        return;
    }
    let (slot, retry) = match component {
        ManagedComponent::Server => (&mut state.server, &mut state.server_retry),
        ManagedComponent::Renderer => (&mut state.renderer, &mut state.renderer_retry),
    };
    let executable = component.executable();
    let now = Instant::now();
    if let Some(child) = slot {
        match child.try_wait() {
            Ok(None) => {
                retry.healthy(now);
                return;
            }
            Ok(Some(status)) => {
                retry.healthy(now);
                diagnostic(&format!("{executable} exited: {status}"));
                *slot = None;
                retry.failed(now);
            }
            Err(error) => {
                if retry.ready(now) {
                    diagnostic(&format!("Cannot inspect {executable}: {error}"));
                    retry.failed(now);
                }
                return;
            }
        }
    }
    if !retry.ready(now) {
        return;
    }
    // 部署 UI 仍可能持有 Rime 数据锁，server 不得与其并发启动。
    if component == ManagedComponent::Server && SingleInstance::acquire("server").is_err() {
        retry.failed(now);
        diagnostic("Server restart deferred: server instance lock unavailable");
        return;
    }
    let directory = state.paths.executable_directory.clone();
    drop(guard);

    let started = start_child(&directory, executable, &[]);
    let mut guard = shared.lock().unwrap_or_else(|error| error.into_inner());
    let state = &mut *guard;
    if is_stopping() || !component.is_monitored(state.operation) {
        return;
    }
    let (slot, retry) = match component {
        ManagedComponent::Server => (&mut state.server, &mut state.server_retry),
        ManagedComponent::Renderer => (&mut state.renderer, &mut state.renderer_retry),
    };
    match started {
        Ok(child) => {
            diagnostic(&format!(
                "monitor restarted {executable} pid={}",
                child.id()
            ));
            *slot = Some(child);
            retry.started(now);
        }
        Err(error) => {
            retry.failed(now);
            diagnostic(&format!("Failed to restart {executable}: {error}"));
        }
    }
}

/// 请求全部受管组件退出；单个组件失败不会阻止另一组件的关闭。
pub(crate) fn shutdown_all() {
    let Some(state) = BROKER_STATE.get() else {
        return;
    };
    let (mut server, mut renderer) = {
        let mut state = state.lock().unwrap_or_else(|error| error.into_inner());
        state.operation = Operation::Shutdown;
        (state.server.take(), state.renderer.take())
    };
    for (name, child, pipe) in [
        ("server", &mut server, default_pipe_name()),
        ("renderer", &mut renderer, default_renderer_pipe_name()),
    ] {
        if let Err(error) = shutdown_component(child, pipe, "broker is exiting") {
            let message = format!("Graceful {name} shutdown failed; process left running: {error}");
            eprintln!("{message}");
            diagnostic(&message);
        }
    }
}

/// 为部署操作请求 server 优雅退出。
pub(crate) fn shutdown_server(services: &mut OperationServices) -> Result<(), String> {
    shutdown_component(
        &mut services.server,
        default_pipe_name(),
        "broker is preparing to deploy Rime data",
    )
}

/// 请求一个受管组件通过 RPC 关闭，并等待其进程退出。
///
/// 不执行强制终止：若组件仍在写入用户数据，调用方应取消当前操作而不是与它竞争。
pub(crate) fn shutdown_component(
    child: &mut Option<Child>,
    pipe: String,
    reason: &str,
) -> Result<(), String> {
    let Some(process) = child.as_mut() else {
        return Ok(());
    };
    if process
        .try_wait()
        .map_err(|error| error.to_string())?
        .is_some()
    {
        *child = None;
        return Ok(());
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .map_err(|error| error.to_string())?;
    let response = runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(5), async {
            let client = RpcClient::connect_as(pipe, PeerRole::Broker)
                .await
                .map_err(|error| error.to_string())?;
            process.verify(&client).await?;
            crate::service_rpc::shutdown(&client, reason)
                .await
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|_| "component shutdown RPC timed out after 5 seconds".to_owned())?
    });
    let response = match response {
        Ok(response) => response,
        Err(error) => {
            shutdown_error_result(
                error,
                process
                    .try_wait()
                    .map(|status| status.is_some())
                    .map_err(|error| error.to_string()),
            )?;
            *child = None;
            return Ok(());
        }
    };
    if !response.accepted {
        return Err(response.message);
    }
    if process
        .wait_timeout(Duration::from_secs(5))
        .map_err(|error| error.to_string())?
        .is_some()
    {
        *child = None;
        return Ok(());
    }
    Err("component did not exit after shutdown acknowledgement".to_owned())
}

/// 确认刚完成启动握手的 server 仍在运行。
pub(crate) fn wait_for_server(child: &mut Child) -> Result<(), String> {
    match child.try_wait().map_err(|error| error.to_string())? {
        None => Ok(()),
        Some(status) => Err(format!("server exited after readiness: {status}")),
    }
}

#[cfg(test)]
#[path = "../tests/unit/service_supervisor.rs"]
mod tests;
