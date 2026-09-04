use std::{
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    sync::{Arc, Mutex, OnceLock},
    thread,
    time::{Duration, Instant},
};

use crate::bindings::*;
use crate::lifecycle::{
    Operation, RestartBackoff, completion_result, shutdown_error_result, workflow_result,
};
use weasel_common::{
    logging::ComponentLogger,
    message::PeerRole,
    process::SingleInstance,
    rpc::{RpcClient, default_pipe_name, default_renderer_pipe_name},
    runtime_paths::RuntimePaths,
};
use windows_strings::{HSTRING, PCWSTR, w};

use weasel_common::broker_menu;
const TRAY_CALLBACK_MESSAGE: u32 = WM_APP as u32 + 1;
const DEPLOY_COMPLETE: u32 = WM_APP as u32 + 2;
static DEPLOYING: AtomicBool = AtomicBool::new(false);
static DEPLOY_RESULT: Mutex<Option<(String, u32)>> = Mutex::new(None);
static OPERATION_THREAD: Mutex<Option<thread::JoinHandle<()>>> = Mutex::new(None);
static STOPPING: AtomicBool = AtomicBool::new(false);
static LOGGER: OnceLock<ComponentLogger> = OnceLock::new();
static TASKBAR_CREATED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

static BROKER_STATE: OnceLock<Arc<Mutex<BrokerState>>> = OnceLock::new();

struct BrokerState {
    directory: PathBuf,
    server: Option<Child>,
    renderer: Option<Child>,
    server_retry: RestartBackoff,
    renderer_retry: RestartBackoff,
    operation: Operation,
}

struct TrayIcon {
    window: HWND,
    data: NOTIFYICONDATAW,
}

impl Drop for TrayIcon {
    fn drop(&mut self) {
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE as u32, &self.data);
            let _ = DestroyWindow(self.window);
        }
    }
}

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let _instance = match SingleInstance::acquire("broker") {
        Ok(instance) => instance,
        Err(error) => {
            eprintln!("weasel-broker: {error}");
            return Ok(());
        }
    };
    let paths = RuntimePaths::discover()?;
    paths.ensure()?;
    let _ = LOGGER.set(ComponentLogger::for_paths(&paths, "broker")?);
    let directory = paths.executable_directory;
    let server = start_child(&directory, "weasel-server.exe", &[])?;
    let renderer = match start_child(&directory, "weasel-renderer.exe", &[]) {
        Ok(renderer) => renderer,
        Err(error) => {
            let mut server = Some(server);
            if let Err(error) =
                shutdown_component(&mut server, default_pipe_name(), "broker startup failed")
            {
                deployment_diagnostic(
                    &directory,
                    &format!("Server left running after startup failure: {error}"),
                );
            }
            return Err(error.into());
        }
    };
    let state = Arc::new(Mutex::new(BrokerState {
        directory: directory.clone(),
        server: Some(server),
        renderer: Some(renderer),
        server_retry: RestartBackoff::new(Instant::now()),
        renderer_retry: RestartBackoff::new(Instant::now()),
        operation: Operation::Idle,
    }));
    let _ = BROKER_STATE.set(Arc::clone(&state));

    let stop_monitor = Arc::new(AtomicBool::new(false));
    let monitor_state = Arc::clone(&state);
    let monitor_stop = Arc::clone(&stop_monitor);
    let monitor = thread::spawn(move || monitor_children(monitor_state, monitor_stop));

    let tray = match create_tray() {
        Ok(tray) => tray,
        Err(error) => {
            stop_monitor.store(true, Ordering::Release);
            let _ = monitor.join();
            shutdown_broker(&state);
            return Err(error);
        }
    };
    message_loop();

    stop_monitor.store(true, Ordering::Release);
    STOPPING.store(true, Ordering::Release);
    let _ = monitor.join();
    // Operations observe STOPPING while awaiting deployment/lock release.
    if let Some(worker) = OPERATION_THREAD.lock().unwrap().take() {
        if worker.join().is_err() {
            deployment_diagnostic(&directory, "Operation worker panicked during shutdown");
        }
    }
    shutdown_broker(&state);
    drop(tray);
    Ok(())
}

fn start_child(
    directory: &std::path::Path,
    executable: &str,
    arguments: &[&str],
) -> Result<Child, std::io::Error> {
    let mut command = Command::new(directory.join(executable));
    command
        .args(arguments)
        .current_dir(directory)
        .stdin(Stdio::null());
    if !weasel_common::runtime_paths::is_development_directory(directory) {
        command.stdout(Stdio::null()).stderr(Stdio::null());
    }
    command.spawn()
}

fn shutdown_broker(state: &Mutex<BrokerState>) {
    let (directory, mut server, mut renderer) = {
        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
        state.operation = Operation::Shutdown;
        (
            state.directory.clone(),
            state.server.take(),
            state.renderer.take(),
        )
    };
    for (name, child, pipe) in [
        ("server", &mut server, default_pipe_name()),
        ("renderer", &mut renderer, default_renderer_pipe_name()),
    ] {
        if let Err(error) = shutdown_component(child, pipe, "broker is exiting") {
            let message = format!("Graceful {name} shutdown failed; process left running: {error}");
            eprintln!("{message}");
            deployment_diagnostic(&directory, &message);
        }
    }
}

fn monitor_children(state: Arc<Mutex<BrokerState>>, stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::Acquire) {
        thread::sleep(Duration::from_millis(500));
        let mut state = state.lock().expect("broker state mutex poisoned");
        if stop.load(Ordering::Acquire) || state.operation == Operation::Shutdown {
            break;
        }
        if state.operation.monitors_server() {
            monitor_child(&mut state, true);
        }
        if state.operation.monitors_renderer() {
            monitor_child(&mut state, false);
        }
    }
}

fn monitor_child(state: &mut BrokerState, server: bool) {
    let (slot, retry, executable) = if server {
        (
            &mut state.server,
            &mut state.server_retry,
            "weasel-server.exe",
        )
    } else {
        (
            &mut state.renderer,
            &mut state.renderer_retry,
            "weasel-renderer.exe",
        )
    };
    let now = Instant::now();
    if let Some(child) = slot {
        match child.try_wait() {
            Ok(None) => {
                retry.healthy(now);
                return;
            }
            Ok(Some(status)) => {
                deployment_diagnostic(&state.directory, &format!("{executable} exited: {status}"));
                *slot = None;
                retry.failed(now);
            }
            Err(error) => {
                if retry.ready(now) {
                    deployment_diagnostic(
                        &state.directory,
                        &format!("Cannot inspect {executable}: {error}"),
                    );
                    retry.failed(now);
                }
                return;
            }
        }
    }
    if !retry.ready(now) {
        return;
    }
    // A detached deploy worker may still own the Rime data. Do not race it.
    if server && SingleInstance::acquire("server").is_err() {
        retry.failed(now);
        deployment_diagnostic(
            &state.directory,
            "Server restart deferred: server instance lock unavailable",
        );
        return;
    }
    match start_child(&state.directory, executable, &[]) {
        Ok(child) => {
            deployment_diagnostic(
                &state.directory,
                &format!("monitor restarted {executable} pid={}", child.id()),
            );
            *slot = Some(child);
            retry.started(now);
        }
        Err(error) => {
            retry.failed(now);
            deployment_diagnostic(
                &state.directory,
                &format!("Failed to restart {executable}: {error}"),
            );
        }
    }
}

fn shutdown_server(state: &mut BrokerState) -> Result<(), String> {
    shutdown_component(
        &mut state.server,
        default_pipe_name(),
        "broker is preparing to deploy Rime data",
    )
}

fn shutdown_component(child: &mut Option<Child>, pipe: String, reason: &str) -> Result<(), String> {
    let Some(server) = child.as_mut() else {
        return Ok(());
    };
    if server.try_wait().map_err(|e| e.to_string())?.is_some() {
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
            client
                .shutdown(reason)
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
                server
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
    for _ in 0..50 {
        if server
            .try_wait()
            .map_err(|error| error.to_string())?
            .is_some()
        {
            *child = None;
            return Ok(());
        }
        thread::sleep(Duration::from_millis(100));
    }
    Err("component did not exit after shutdown acknowledgement".to_owned())
}

fn deployment_diagnostic(_directory: &std::path::Path, text: &str) {
    use std::io::Write;
    if let Some(logger) = LOGGER.get() {
        let mut logger = logger.clone();
        if let Err(error) = writeln!(logger, "[broker {:?}] {text}", std::time::SystemTime::now()) {
            eprintln!("weasel-broker: diagnostic write failed: {error}");
        }
    }
}

fn wait_for_server(child: &mut Child) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    runtime.block_on(async {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            if STOPPING.load(Ordering::Acquire) {
                return Err("broker is shutting down".into());
            }
            if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                return Err(format!(
                    "server pid={} exited before readiness: {status}",
                    child.id()
                ));
            }
            let probe = tokio::time::timeout(Duration::from_millis(300), async {
                let client = RpcClient::connect_as(default_pipe_name(), PeerRole::Broker).await?;
                client.ping("broker readiness probe").await
            })
            .await;
            if matches!(probe, Ok(Ok(_))) {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(format!(
                    "server pid={} did not answer ping within 10 seconds",
                    child.id()
                ));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
}

async fn bounded_operation<T>(
    future: impl std::future::Future<Output = Result<T, String>>,
    limit: Duration,
) -> Result<T, String> {
    let deadline = Instant::now() + limit;
    let mut future = std::pin::pin!(future);
    loop {
        if STOPPING.load(Ordering::Acquire) {
            return Err("broker shutdown interrupted deployment observation; deployment process left running".into());
        }
        if Instant::now() >= deadline {
            return Err("deployment observation timed out; deployment process left running".into());
        }
        if let Ok(result) = tokio::time::timeout(Duration::from_millis(100), future.as_mut()).await
        {
            return result;
        }
    }
}

fn deploy(state: &mut BrokerState) -> (String, u32) {
    let result = (|| -> Result<(), String> {
        shutdown_server(state)?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        runtime.block_on(async {
            use std::io::Write;
            use tokio::io::AsyncReadExt;
            use weasel_common::deploy_protocol::wait_for_completion;
            let paths =
                RuntimePaths::for_directory(state.directory.clone()).map_err(|e| e.to_string())?;
            let development = paths.development;
            let log = Arc::new(Mutex::new(
                ComponentLogger::for_paths(&paths, "broker-deploy").map_err(|e| e.to_string())?,
            ));
            let mut child = tokio::process::Command::new(state.directory.join("weasel-server.exe"))
                .arg("--deploy-ui")
                .current_dir(&state.directory)
                .creation_flags(CREATE_NO_WINDOW as u32)
                .kill_on_drop(false)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(if development {
                    Stdio::piped()
                } else {
                    Stdio::null()
                })
                .spawn()
                .map_err(|e| format!("could not start deployment UI: {e}"))?;
            let _ = writeln!(
                log.lock().unwrap(),
                "[broker] deployment UI started pid={:?}",
                child.id()
            );
            let mut stdout = child.stdout.take().unwrap();
            let stderr = child.stderr.take();
            let stderr_log = log.clone();
            let errors = tokio::spawn(async move {
                let Some(mut stderr) = stderr else {
                    return;
                };
                let mut bytes = [0; 8192];
                while let Ok(count) = stderr.read(&mut bytes).await {
                    if count == 0 {
                        break;
                    }
                    let _ = stderr_log.lock().unwrap().write_all(&bytes[..count]);
                }
            });
            let result = bounded_operation(
                async {
                    let done = wait_for_completion(&mut stdout, |message| {
                        if development {
                            log.lock().unwrap().write_all(message.text.as_bytes())
                        } else {
                            Ok(())
                        }
                    })
                    .await
                    .map_err(|e| e.to_string())?;
                    let _ = writeln!(
                        log.lock().unwrap(),
                        "\n[broker] received Complete success={} exit_code={:?}; detaching UI",
                        done.success,
                        done.exit_code
                    );
                    log.lock()
                        .unwrap()
                        .write_all(format!("\n{}\n", done.message).as_bytes())
                        .map_err(|e| e.to_string())?;
                    completion_result(done.success, done.exit_code, &done.message)
                },
                Duration::from_secs(300),
            )
            .await;
            errors.abort();
            let _ = errors.await;
            let _ = log.lock().unwrap().flush();
            // Explicitly detach: do not wait for the user's OK button.
            drop(child);
            result
        })?;
        Ok(())
    })();
    let mut restoration = Ok(());
    if state.server.is_none() {
        // If the UI crashed, its hidden worker may still be finishing Rime.
        // Never race that worker by starting a service against the same files.
        if !STOPPING.load(Ordering::Acquire) {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                match SingleInstance::acquire("server") {
                    Ok(guard) => {
                        drop(guard);
                        break;
                    }
                    Err(weasel_common::process::SingleInstanceError::AlreadyRunning) => {
                        if STOPPING.load(Ordering::Acquire) || Instant::now() >= deadline {
                            let error = workflow_result(
                                result,
                                Err(
                                    "server instance lock remained busy; automatic retry deferred"
                                        .into(),
                                ),
                            )
                            .unwrap_err();
                            deployment_diagnostic(&state.directory, &error);
                            return (error, MB_ICONERROR as u32);
                        }
                        thread::sleep(Duration::from_millis(100))
                    }
                    Err(lock_error) => {
                        let error = workflow_result(
                            result,
                            Err(format!("无法确认部署进程已释放数据：{lock_error}")),
                        )
                        .unwrap_err();
                        deployment_diagnostic(&state.directory, &error);
                        return (error, MB_ICONERROR as u32);
                    }
                }
            }
        }
        if STOPPING.load(Ordering::Acquire) {
            let error =
                workflow_result(result, Err("restoration skipped: broker is exiting".into()))
                    .unwrap_err();
            deployment_diagnostic(&state.directory, &error);
            return (error, MB_ICONERROR as u32);
        }
        match start_child(&state.directory, "weasel-server.exe", &[]) {
            Ok(mut child) => {
                deployment_diagnostic(
                    &state.directory,
                    &format!("ordinary server spawned pid={}", child.id()),
                );
                state.server_retry.started(Instant::now());
                match wait_for_server(&mut child) {
                    Ok(()) => deployment_diagnostic(
                        &state.directory,
                        "ordinary server answered readiness ping",
                    ),
                    Err(restart) => {
                        deployment_diagnostic(&state.directory, &restart);
                        restoration = Err(restart);
                    }
                }
                state.server = Some(child);
            }
            Err(restart) => {
                restoration = Err(format!("server restart failed: {restart}"));
            }
        }
    }
    match workflow_result(result, restoration) {
        Err(error) => {
            deployment_diagnostic(
                &state.directory,
                &format!("deployment workflow failed: {error}"),
            );
            (format!("部署流程异常：\n{error}"), MB_ICONERROR as u32)
        }
        Ok(()) => (String::new(), 0),
    }
}

fn restart_components(state: &mut BrokerState) -> (String, u32) {
    let mut errors = Vec::new();
    if let Err(error) = shutdown_component(
        &mut state.server,
        default_pipe_name(),
        "broker requested restart",
    ) {
        // Do not forcibly terminate Rime while it might be writing user data.
        return (
            format!("无法停止 server，已取消重启：{error}"),
            MB_ICONERROR as u32,
        );
    }
    if let Err(error) = shutdown_component(
        &mut state.renderer,
        default_renderer_pipe_name(),
        "broker requested restart",
    ) {
        errors.push(format!("无法停止 renderer：{error}"));
    }
    // Restore each component that actually stopped, even on partial failure.
    if state.renderer.is_none() && !STOPPING.load(Ordering::Acquire) {
        match start_child(&state.directory, "weasel-renderer.exe", &[]) {
            Ok(child) => {
                state.renderer = Some(child);
                state.renderer_retry.started(Instant::now());
            }
            Err(error) => errors.push(format!("无法启动 renderer：{error}")),
        }
    }
    if state.server.is_none() && !STOPPING.load(Ordering::Acquire) {
        match start_child(&state.directory, "weasel-server.exe", &[]) {
            Ok(mut child) => {
                if let Err(error) = wait_for_server(&mut child) {
                    errors.push(error);
                }
                state.server = Some(child);
                state.server_retry.started(Instant::now());
            }
            Err(error) => errors.push(format!("无法启动 server：{error}")),
        }
    }
    if errors.is_empty() {
        (String::new(), 0)
    } else {
        (
            format!("重启流程异常：\n{}", errors.join("\n")),
            MB_ICONERROR as u32,
        )
    }
}

fn begin_deploy(window: HWND, operation: Operation) {
    if DEPLOYING.swap(true, Ordering::AcqRel) {
        return;
    }
    let Some(state) = BROKER_STATE.get().cloned() else {
        DEPLOYING.store(false, Ordering::Release);
        return;
    };
    let hwnd = window.0 as usize;
    if let Some(previous) = OPERATION_THREAD.lock().unwrap().take() {
        let _ = previous.join();
    }
    let started = thread::Builder::new()
        .name("weasel-deploy".to_owned())
        .spawn(move || {
            // Move only the affected children out. No global lock spans RPC,
            // deployment telemetry, readiness, or instance-lock waits.
            let mut owned = {
                let mut shared = state.lock().unwrap_or_else(|e| e.into_inner());
                shared.operation = operation;
                BrokerState {
                    directory: shared.directory.clone(),
                    server: shared.server.take(),
                    renderer: if operation == Operation::Restart {
                        shared.renderer.take()
                    } else {
                        None
                    },
                    server_retry: RestartBackoff::new(Instant::now()),
                    renderer_retry: RestartBackoff::new(Instant::now()),
                    operation,
                }
            };
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match operation {
                    Operation::Restart => restart_components(&mut owned),
                    _ => deploy(&mut owned),
                }))
                .unwrap_or_else(|_| {
                    (
                        "Broker operation panicked; child ownership restored".into(),
                        MB_ICONERROR as u32,
                    )
                });
            if !result.0.is_empty() {
                deployment_diagnostic(&owned.directory, &result.0);
            }
            {
                let mut shared = state.lock().unwrap_or_else(|e| e.into_inner());
                shared.server = owned.server;
                shared.server_retry.started(Instant::now());
                if operation == Operation::Restart {
                    shared.renderer = owned.renderer;
                    shared.renderer_retry.started(Instant::now());
                }
                shared.operation = Operation::Idle;
            }
            *DEPLOY_RESULT.lock().unwrap() = Some(result);
            post_deploy_complete(HWND(hwnd as *mut _));
        });
    match started {
        Ok(worker) => *OPERATION_THREAD.lock().unwrap() = Some(worker),
        Err(error) => {
            *DEPLOY_RESULT.lock().unwrap() =
                Some((format!("无法启动后台操作：{error}"), MB_ICONERROR as u32));
            post_deploy_complete(window);
        }
    }
}

fn post_deploy_complete(window: HWND) {
    if !unsafe { PostMessageW(Some(window), DEPLOY_COMPLETE, WPARAM(0), LPARAM(0)) }.as_bool() {
        eprintln!(
            "weasel-broker: could not post deployment completion: {}",
            std::io::Error::last_os_error()
        );
        // Keep the result available so the next menu interaction can display it.
    }
}

fn show_deploy_result(window: HWND) {
    let result = DEPLOY_RESULT.lock().unwrap().take();
    let Some((text, icon)) = result else {
        return;
    };
    if text.is_empty() {
        DEPLOYING.store(false, Ordering::Release);
        return;
    }
    let title = w!("weasel-rs");
    let text = HSTRING::from(text);
    unsafe {
        let result = MessageBoxW(
            Some(window),
            PCWSTR(text.as_ptr()),
            title,
            MB_OK as u32 | icon | MB_SETFOREGROUND as u32,
        );
        if result == 0 {
            eprintln!(
                "weasel-broker: MessageBoxW failed: {}",
                std::io::Error::last_os_error()
            );
        }
    }
    DEPLOYING.store(false, Ordering::Release);
}

fn create_tray() -> Result<TrayIcon, Box<dyn std::error::Error>> {
    let taskbar = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
    if taskbar == 0 {
        return Err("RegisterWindowMessageW(TaskbarCreated) failed".into());
    }
    TASKBAR_CREATED.store(taskbar, Ordering::Release);
    let class_name = HSTRING::from(broker_menu::WINDOW_CLASS);
    let title = w!("weasel-rs");
    let hinstance = unsafe { GetModuleHandleW(None) };
    let window_class = WNDCLASSW {
        lpfnWndProc: Some(window_proc),
        hInstance: hinstance,
        lpszClassName: PCWSTR(class_name.as_ptr()),
        ..Default::default()
    };
    unsafe {
        let _ = RegisterClassW(&window_class);
    }
    let window = unsafe {
        CreateWindowExW(
            Default::default(),
            PCWSTR(class_name.as_ptr()),
            title,
            Default::default(),
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinstance),
            None,
        )
    };
    if window.0.is_null() {
        return Err("CreateWindowExW failed".into());
    }
    match add_tray_icon(window) {
        Ok(data) => Ok(TrayIcon { window, data }),
        Err(error) => {
            unsafe {
                let _ = DestroyWindow(window);
            }
            Err(error)
        }
    }
}

fn add_tray_icon(window: HWND) -> Result<NOTIFYICONDATAW, Box<dyn std::error::Error>> {
    let hinstance = unsafe { GetModuleHandleW(None) };
    let icon = unsafe { LoadIconW(Some(hinstance), w!("WEASEL_ICON")) };
    let mut data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: window,
        uID: 1,
        uFlags: (NIF_MESSAGE | NIF_ICON | NIF_TIP) as u32,
        uCallbackMessage: TRAY_CALLBACK_MESSAGE,
        hIcon: icon,
        ..Default::default()
    };
    let tip = HSTRING::from("Weasel-RS 服务器");
    let length = tip.len().min(data.szTip.len() - 1);
    data.szTip[..length].copy_from_slice(&tip[..length]);
    if !unsafe { Shell_NotifyIconW(NIM_ADD as u32, &data).as_bool() } {
        return Err("Shell_NotifyIconW(NIM_ADD) failed".into());
    }
    Ok(data)
}

fn message_loop() {
    let mut message = MSG::default();
    unsafe {
        loop {
            let result = GetMessageW(&mut message, None, 0, 0).0;
            if result <= 0 {
                if result < 0 {
                    eprintln!(
                        "weasel-broker: GetMessageW failed: {}",
                        std::io::Error::last_os_error()
                    );
                }
                break;
            }
            let _ = TranslateMessage(&message);
            let _ = DispatchMessageW(&message);
        }
    }
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message != 0 && message == TASKBAR_CREATED.load(Ordering::Acquire) {
        if let Err(error) = add_tray_icon(window) {
            eprintln!("weasel-broker: could not restore tray after Explorer restart: {error}");
        }
        return LRESULT(0);
    }
    match message {
        DEPLOY_COMPLETE => show_deploy_result(window),
        TRAY_CALLBACK_MESSAGE if lparam.0 as u32 == WM_RBUTTONUP as u32 => show_menu(window),
        message if message == WM_COMMAND as u32 => {
            handle_command(window, (wparam.0 & 0xffff) as u32)
        }
        message if message == WM_DESTROY as u32 => unsafe { PostQuitMessage(0) },
        _ => return unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
    LRESULT(0)
}

fn show_menu(window: HWND) {
    if DEPLOY_RESULT.lock().unwrap().is_some() {
        show_deploy_result(window);
        return;
    }
    let menu = unsafe { CreatePopupMenu() };
    let busy = DEPLOYING.load(Ordering::Acquire);
    unsafe {
        for &(id, label) in broker_menu::ITEMS {
            let disabled = busy && matches!(id, broker_menu::DEPLOY | broker_menu::RESTART);
            let flags = if id == 0 {
                MF_SEPARATOR as u32
            } else {
                MF_STRING as u32
            } | if disabled { MF_GRAYED as u32 } else { 0 };
            let label = HSTRING::from(label);
            let _ = AppendMenuW(menu, flags, id as usize, PCWSTR(label.as_ptr()));
        }
        let _ = SetForegroundWindow(window);
        let mut point = POINT::default();
        let _ = GetCursorPos(&mut point);
        let command = TrackPopupMenu(
            menu,
            (TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY) as u32,
            point.x,
            point.y,
            Some(0),
            window,
            None,
        );
        let _ = DestroyMenu(menu);
        let _ = PostMessageW(Some(window), WM_NULL as u32, WPARAM(0), LPARAM(0));
        if command.0 != 0 {
            let _ = PostMessageW(
                Some(window),
                WM_COMMAND as u32,
                WPARAM(command.0 as usize),
                LPARAM(0),
            );
        }
    }
}

fn handle_command(window: HWND, command: u32) {
    match command {
        broker_menu::DEPLOY => begin_deploy(window, Operation::Deploy),
        broker_menu::RESTART => begin_deploy(window, Operation::Restart),
        broker_menu::EXIT => {
            STOPPING.store(true, Ordering::Release);
            unsafe { PostQuitMessage(0) };
        }
        id if broker_menu::is_command(id) => crate::menu_actions::open(id),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_child_shutdown_needs_no_rpc() {
        assert!(shutdown_component(&mut None, "unused-test-pipe".into(), "test").is_ok());
    }

    #[test]
    fn deployment_observation_has_a_deadline() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let result = runtime.block_on(bounded_operation(
            std::future::pending::<Result<(), String>>(),
            Duration::ZERO,
        ));
        assert!(result.unwrap_err().contains("timed out"));
    }

    #[test]
    fn deployment_observation_preserves_reported_failure() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let result = runtime.block_on(bounded_operation(
            async { Err::<(), _>("worker failed".to_owned()) },
            Duration::from_secs(1),
        ));
        assert_eq!(result.unwrap_err(), "worker failed");
    }
}
