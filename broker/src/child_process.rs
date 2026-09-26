//! 管理由 broker 启动的服务子进程，并验证就绪服务确实属于当前安装和启动者。
//!
//! Server 使用标准子进程句柄，renderer 通过 Shell 激活并保留其真实进程句柄；
//! 两条启动路径最终都经过所有权握手，并由同一个 [`Child`] 管理退出和身份复核。
use crate::bindings::*;
use std::os::windows::{
    io::{AsRawHandle, FromRawHandle, IntoRawHandle, OwnedHandle, RawHandle},
    process::ExitStatusExt,
};
use std::{
    io,
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    sync::OnceLock,
    time::Duration,
};
use weasel_common::{
    comrt::ComApartment,
    message::PeerRole,
    process::{SingleInstance, SingleInstanceError},
    rpc::{RpcClient, default_pipe_name, default_renderer_pipe_name},
    service_owner::{self, StopSignal},
};
use windows_strings::{HSTRING, w};

/// 通过 Shell 启动 renderer，并返回其真实进程句柄及 PID。
///
/// 激活在临时 STA 线程中同步执行，不改变调用线程的 COM apartment。
fn spawn_shell(
    file: PathBuf,
    directory: PathBuf,
    args: Vec<String>,
) -> io::Result<(OwnedHandle, u32)> {
    std::thread::Builder::new()
        .name("renderer-launch".into())
        .spawn(move || unsafe {
            let _apartment = ComApartment::initialize_sta().map_err(io::Error::other)?;
            let file = HSTRING::from(file.as_os_str());
            let directory = HSTRING::from(directory.as_os_str());
            let parameters = HSTRING::from(
                args.iter()
                    .map(|argument| quote(argument))
                    .collect::<Vec<_>>()
                    .join(" "),
            );
            let mut info = SHELLEXECUTEINFOW {
                cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
                fMask: (SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI) as u32,
                lpVerb: w!("open"),
                lpFile: windows_core::PCWSTR(file.as_ptr()),
                lpParameters: windows_core::PCWSTR(parameters.as_ptr()),
                lpDirectory: windows_core::PCWSTR(directory.as_ptr()),
                nShow: SW_SHOWNORMAL,
                ..Default::default()
            };
            if !ShellExecuteExW(&mut info).as_bool() {
                return Err(io::Error::last_os_error());
            }
            if info.hProcess.0.is_null() {
                return Err(io::Error::other(
                    "Shell did not return renderer process handle",
                ));
            }
            let handle = OwnedHandle::from_raw_handle(info.hProcess.0);
            let pid = GetProcessId(HANDLE(handle.as_raw_handle()));
            if pid == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok((handle, pid))
        })?
        .join()
        .map_err(|_| io::Error::other("renderer launch thread panicked"))?
}

/// 等待指定进程句柄，并在其退出后读取退出码。
///
/// 超时返回 `Ok(None)`；Windows 等待或读取退出码失败时返回最后一个系统错误。
fn wait(handle: &OwnedHandle, timeout: u32) -> io::Result<Option<ExitStatus>> {
    unsafe {
        let status = WaitForSingleObject(HANDLE(handle.as_raw_handle()), timeout) as i32;
        match status {
            WAIT_OBJECT_0 => {
                let mut code = 0;
                if !GetExitCodeProcess(HANDLE(handle.as_raw_handle()), &mut code).as_bool() {
                    return Err(io::Error::last_os_error());
                }
                Ok(Some(ExitStatus::from_raw(code)))
            }
            WAIT_TIMEOUT => Ok(None),
            _ => Err(io::Error::last_os_error()),
        }
    }
}

/// 按 Windows 命令行参数规则为单个参数加引号并转义反斜杠和双引号。
///
/// 输出始终带外围双引号；引号前和参数末尾的反斜杠需要加倍，避免子进程解析时
/// 改变参数边界或内容。
fn quote(value: &str) -> String {
    let mut result = String::from("\"");
    let mut slashes = 0;
    for ch in value.chars() {
        if ch == '\\' {
            slashes += 1;
            continue;
        }
        result.extend(std::iter::repeat_n(
            '\\',
            if ch == '"' { slashes * 2 + 1 } else { slashes },
        ));
        slashes = 0;
        result.push(ch);
    }
    result.extend(std::iter::repeat_n('\\', slashes * 2));
    result.push('"');
    result
}

struct Owner {
    /// broker 创建时的进程出生时间，用于子进程验证所有者身份并规避 PID 重用。
    birth: u64,
}

/// 当前 broker 的进程身份；首次初始化后在进程生命周期内保持不变。
static OWNER: OnceLock<Owner> = OnceLock::new();

/// 记录当前 broker 的进程出生时间，供受管服务核验其所有者。
///
/// 重复调用不会替换已记录的身份；打开当前进程或读取出生时间失败时返回 I/O 错误。
pub fn initialize() -> io::Result<()> {
    let process = service_owner::open_process(std::process::id())?;
    let _ = OWNER.set(Owner {
        birth: service_owner::birth(&process)?,
    });
    Ok(())
}

/// 将受支持的可执行文件名映射为服务身份中的组件名。
fn component(executable: &str) -> io::Result<&'static str> {
    match executable {
        "weasel-server.exe" => Ok("server"),
        "weasel-renderer.exe" => Ok("renderer"),
        _ => Err(io::Error::other("not a managed service")),
    }
}

/// 返回指定组件用于 RPC 握手的默认命名管道名称。
fn pipe(component: &str) -> String {
    if component == "server" {
        default_pipe_name()
    } else {
        default_renderer_pipe_name()
    }
}

/// 创建带完整 I/O 驱动的单线程 Tokio 运行时，供同步生命周期入口执行异步 RPC。
fn runtime() -> io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
}

/// 已通过启动身份握手、由 broker 监督的服务子进程。
///
/// 丢弃该值会发送协作式停止信号；进程句柄仍用于观察退出和核验实际进程身份。
pub struct Child {
    /// 可等待并用于核验身份的进程内核句柄。
    handle: OwnedHandle,
    /// 启动时从进程对象读取的系统进程 ID。
    pid: u32,
    /// 与该次启动令牌绑定的停止信号。
    stop: StopSignal,
    /// 经核验的服务组件名。
    component: &'static str,
    /// 就绪握手确认的服务实例标识。
    instance: String,
    /// 本次启动生成并传给服务的所有者令牌。
    token: String,
}

impl Child {
    /// 返回受管子进程的系统进程 ID。
    pub fn id(&self) -> u32 {
        self.pid
    }

    /// 非阻塞地检查子进程；仍在运行时返回 `Ok(None)`。
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        wait(&self.handle, 0)
    }

    /// 在有限时间内等待子进程退出；超时返回 `Ok(None)`。
    pub fn wait_timeout(&mut self, timeout: Duration) -> io::Result<Option<ExitStatus>> {
        const MAX_FINITE_TIMEOUT_MS: u128 = u32::MAX as u128 - 1;
        let timeout = timeout.as_millis().min(MAX_FINITE_TIMEOUT_MS) as u32;
        wait(&self.handle, timeout)
    }

    /// 重新核验 RPC 端点对应的进程、组件、启动令牌和服务实例。
    pub async fn verify(&self, client: &RpcClient) -> Result<(), String> {
        let identity = crate::service_rpc::identify(client)
            .await
            .map_err(|error| error.to_string())?;
        if !matches_identity(
            &identity,
            client.server_pid(),
            self.id(),
            self.component,
            &self.token,
        ) || (!self.instance.is_empty() && self.instance != identity.instance)
        {
            return Err("pipe belongs to a different service instance".into());
        }
        Ok(())
    }
}

impl AsRawHandle for Child {
    fn as_raw_handle(&self) -> RawHandle {
        self.handle.as_raw_handle()
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        self.stop.signal();
    }
}

/// 比较 RPC 报告的服务身份与内核观察到的进程身份及本次启动凭据。
fn matches_identity(
    identity: &weasel_common::message::ServiceIdentity,
    actual: u32,
    expected: u32,
    component: &str,
    token: &str,
) -> bool {
    actual != 0
        && actual == expected
        && identity.pid == actual
        && identity.component == component
        && identity.owner_token == token
        && !identity.instance.is_empty()
}

/// 启动服务并等待其完成所有权及就绪握手。
pub fn start(directory: &Path, executable: &str, arguments: &[&str]) -> io::Result<Child> {
    let component = component(executable)?;

    // 构造启动参数
    let owner = OWNER
        .get()
        .ok_or_else(|| io::Error::other("broker ownership not initialized"))?;
    drop(SingleInstance::acquire(component).map_err(io::Error::other)?);
    let token = service_owner::new_token()?;
    let stop = StopSignal::new(&token)?;
    let mut launch_args = vec![
        "--broker-owner".to_owned(),
        token.clone(),
        std::process::id().to_string(),
        owner.birth.to_string(),
    ];

    let (handle, pid) = if component == "renderer" {
        // renderer 具有 uiAccess 权限，需要通过 shell 启动
        launch_args.extend(arguments.iter().map(|argument| (*argument).to_owned()));
        spawn_shell(
            directory.join(executable),
            directory.to_owned(),
            launch_args,
        )?
    } else {
        let mut command = Command::new(directory.join(executable));
        command
            .args(&launch_args)
            .args(arguments)
            .current_dir(directory)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let process = command.spawn()?;
        let pid = process.id();
        // std::process::Child 在 Windows 上只拥有进程句柄；将所有权转给
        // OwnedHandle 后，两种启动路径即可共用同一套等待和退出码读取逻辑。
        let handle = unsafe { OwnedHandle::from_raw_handle(process.into_raw_handle()) };
        (handle, pid)
    };

    // 构造 Child ，等待进程就绪
    let mut child = Child {
        handle,
        pid,
        stop,
        component,
        instance: String::new(),
        token,
    };
    let result = runtime()?.block_on(async {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(status) = child.try_wait()? {
                    return Err(io::Error::other(format!(
                        "{component} exited before readiness: {status}"
                    )));
                }
                if let Ok(client) = RpcClient::connect_as(pipe(component), PeerRole::Broker).await {
                    if client.server_pid() != child.id() {
                        return Err(io::Error::other(
                            "service pipe was occupied by a different PID",
                        ));
                    }
                    let identity = crate::service_rpc::identify(&client)
                        .await
                        .map_err(io::Error::other)?;
                    if !matches_identity(
                        &identity,
                        client.server_pid(),
                        child.id(),
                        component,
                        &child.token,
                    ) {
                        return Err(io::Error::other("service ownership handshake failed"));
                    }
                    if identity.ready {
                        child.instance = identity.instance;
                        return Ok(());
                    }
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .map_err(|_| io::Error::other(format!("{component} readiness timed out")))?
    });
    if let Err(error) = result {
        child.stop.signal();
        unsafe {
            let _ = WaitForSingleObject(HANDLE(child.as_raw_handle()), 5000);
        }
        return Err(error);
    }
    Ok(child)
}

/// 在启动阶段验证并正常关闭来自同一安装目录的遗留服务。
pub fn clear_stale(directory: &Path, executable: &str) -> io::Result<()> {
    let component = component(executable)?;
    match SingleInstance::acquire(component) {
        Ok(guard) => {
            drop(guard);
            return Ok(());
        }
        Err(SingleInstanceError::AlreadyRunning) => {}
        Err(error) => return Err(io::Error::other(error)),
    }
    let expected = directory.join(executable).canonicalize()?;
    let process = runtime()?.block_on(async {
        tokio::time::timeout(Duration::from_secs(5), async {
            let client = RpcClient::connect_as_with_timeout(
                pipe(component),
                PeerRole::Broker,
                Duration::from_secs(2),
            )
            .await
            .map_err(io::Error::other)?;
            let pid = client.server_pid();
            if pid == 0 {
                return Err(io::Error::other("cannot identify stale pipe owner"));
            }
            let process = service_owner::open_process(pid)?;
            let path = service_owner::process_path(&process)?.canonicalize()?;
            if !path
                .as_os_str()
                .to_string_lossy()
                .eq_ignore_ascii_case(&expected.as_os_str().to_string_lossy())
            {
                return Err(io::Error::other(format!(
                    "{component} belongs to another installation: {}",
                    path.display()
                )));
            }
            if let Ok(Ok(identity)) = tokio::time::timeout(
                Duration::from_millis(500),
                crate::service_rpc::identify(&client),
            )
            .await
            {
                if identity.pid != pid || identity.component != component {
                    return Err(io::Error::other("stale service identity mismatch"));
                }
            }
            drop(client);
            let client = RpcClient::connect_as(pipe(component), PeerRole::Broker)
                .await
                .map_err(io::Error::other)?;
            if client.server_pid() != pid
                || unsafe { WaitForSingleObject(HANDLE(process.as_raw_handle()), 0) }
                    == WAIT_OBJECT_0 as u32
            {
                return Err(io::Error::other("stale service changed during cleanup"));
            }
            let response = crate::service_rpc::shutdown(
                &client,
                "broker startup is replacing an unmanaged service",
            )
            .await
            .map_err(io::Error::other)?;
            if !response.accepted {
                return Err(io::Error::other(response.message));
            }
            Ok(process)
        })
        .await
        .map_err(|_| io::Error::other("stale service shutdown timed out"))?
    })?;
    if unsafe { WaitForSingleObject(HANDLE(process.as_raw_handle()), 5000) } != WAIT_OBJECT_0 as u32
    {
        return Err(io::Error::other(format!(
            "{component} did not exit; startup cancelled"
        )));
    }
    drop(SingleInstance::acquire(component).map_err(io::Error::other)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_shell_arguments() {
        assert_eq!(super::quote(""), "\"\"");
        assert_eq!(super::quote("a b"), "\"a b\"");
        assert_eq!(super::quote("a\"b"), "\"a\\\"b\"");
        assert_eq!(super::quote("Local\\event\\"), "\"Local\\event\\\\\"");
    }

    #[test]
    fn readiness_requires_kernel_pid_owner_and_component() {
        let mut id = weasel_common::message::ServiceIdentity {
            pid: 7,
            component: "server".into(),
            owner_token: "owner".into(),
            instance: "instance".into(),
            ready: true,
        };
        assert!(matches_identity(&id, 7, 7, "server", "owner"));
        assert!(!matches_identity(&id, 8, 7, "server", "owner"));
        assert!(!matches_identity(&id, 7, 7, "renderer", "owner"));
        assert!(!matches_identity(&id, 7, 7, "server", "different"));
        id.instance.clear();
        assert!(!matches_identity(&id, 7, 7, "server", "owner"));
    }
}
