//! Broker 与子服务之间的进程所有权和生命周期协作。
//!
//! 本模块通过启动参数、进程身份和命名事件确认子服务由哪个 Broker 管理，并在
//! Broker 退出时触发子服务的正常关闭流程。
use crate::bindings::*;
use std::{
    io,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    sync::OnceLock,
};

static INSTANCE: OnceLock<String> = OnceLock::new();
struct LaunchOwner {
    token: String,
    pid: u32,
    birth: u64,
}
static LAUNCH_OWNER: OnceLock<LaunchOwner> = OnceLock::new();

/// 解析 Broker 启动时附带的所有权参数，并返回剩余参数。
///
/// Broker 将私有所有权前缀放在命令行开头，使直接启动和 Shell 启动的子服务使用
/// 同一种传输方式。三个值依次为子进程令牌、Broker PID 和 Broker 创建时间。
///
/// # Errors
///
/// 参数前缀不完整、PID 或创建时间格式错误，或所有权信息已初始化时返回错误。
pub fn take_launch_args(
    args: impl IntoIterator<Item = std::ffi::OsString>,
) -> io::Result<Vec<std::ffi::OsString>> {
    let mut args = args.into_iter().peekable();
    if args.peek().is_some_and(|arg| arg == "--broker-owner") {
        args.next();
        let mut next = || {
            args.next()
                .and_then(|arg| arg.into_string().ok())
                .filter(|arg| !arg.is_empty())
                .ok_or_else(|| io::Error::other("incomplete broker ownership arguments"))
        };
        let token = next()?;
        let pid = next()?.parse::<u32>().map_err(io::Error::other)?;
        let birth = next()?.parse::<u64>().map_err(io::Error::other)?;
        LAUNCH_OWNER
            .set(LaunchOwner { token, pid, birth })
            .map_err(|_| io::Error::other("broker ownership already initialized"))?;
    }
    Ok(args.collect())
}

/// 生成一个适合标识单次进程所有权关系的 GUID 字符串。
///
/// # Errors
///
/// GUID 创建失败时返回对应的 Windows 错误。
pub fn new_token() -> io::Result<String> {
    unsafe {
        CoCreateGuid()
            .map(|v| format!("{v:?}"))
            .map_err(io::Error::other)
    }
}

/// 构造当前进程向 Broker 报告的服务身份。
///
/// 所有权令牌来自启动参数；缺失时以空字符串表示无 Broker 所有者。
/// `ready` 由调用方提供，用于区分服务已注册身份与已完成初始化的状态。
pub fn identity(component: &str, ready: bool) -> crate::message::ServiceIdentity {
    crate::message::ServiceIdentity {
        pid: std::process::id(),
        component: component.into(),
        ready,
        owner_token: LAUNCH_OWNER
            .get()
            .map(|owner| owner.token.clone())
            .unwrap_or_default(),
        instance: INSTANCE.get().cloned().unwrap_or_default(),
    }
}

/// 以等待和查询有限信息所需的权限打开进程，并取得拥有所有权的句柄。
///
/// # Errors
///
/// 进程不存在、权限不足或系统调用失败时返回 Windows I/O 错误。
pub fn open_process(pid: u32) -> io::Result<OwnedHandle> {
    let handle = unsafe {
        OpenProcess(
            (SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION) as u32,
            false,
            pid,
        )
    };
    if handle.0.is_null() {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedHandle::from_raw_handle(handle.0) })
    }
}

/// 从进程句柄读取可执行映像的完整路径。
///
/// 调用方须持有仍有效且具备查询权限的进程句柄；返回路径由本函数独立拥有。
///
/// # Errors
///
/// 查询失败时返回 Windows I/O 错误。
pub fn process_path(handle: &OwnedHandle) -> io::Result<std::path::PathBuf> {
    let mut buffer = vec![0u16; 32768];
    let mut length = buffer.len() as u32;
    if !unsafe {
        QueryFullProcessImageNameW(
            HANDLE(handle.as_raw_handle()),
            0,
            windows_core::PWSTR(buffer.as_mut_ptr()),
            &mut length,
        )
    }
    .as_bool()
    {
        return Err(io::Error::last_os_error());
    }
    use std::os::windows::ffi::OsStringExt;
    Ok(std::ffi::OsString::from_wide(&buffer[..length as usize]).into())
}

/// 代表 Broker 管理的子进程停止事件。
///
/// 事件句柄由该值独占持有。显式调用 [`signal`](Self::signal) 或丢弃此值都会置位
/// 事件，使监听方进入正常关闭流程；因此即使子进程启动中途失败，清理该对象也会
/// 通知已经启动的子服务退出。
pub struct StopSignal {
    handle: OwnedHandle,
}
impl StopSignal {
    /// 为指定子进程令牌创建一个手动重置停止事件。
    ///
    /// # Errors
    ///
    /// 身份读取、名称创建、安全描述符构造或事件创建失败时返回错误。
    pub fn new(token: &str) -> io::Result<Self> {
        let name = stop_event_name(token)?;
        let identity = crate::windows_security::RuntimeIdentity::current()?;
        let security = crate::windows_security::LocalSecurityDescriptor::for_logon(&identity)?;
        let attributes = security.security_attributes();
        let wide = windows_strings::HSTRING::from(&name);
        let raw = unsafe {
            CreateEventW(
                Some(&attributes),
                true,
                false,
                windows_core::PCWSTR(wide.as_ptr()),
            )
        };
        if raw.0.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            handle: unsafe { OwnedHandle::from_raw_handle(raw.0) },
        })
    }
    /// 置位停止事件；重复调用是安全的。
    pub fn signal(&self) {
        unsafe {
            let _ = SetEvent(HANDLE(self.handle.as_raw_handle()));
        }
    }
}
impl Drop for StopSignal {
    fn drop(&mut self) {
        self.signal();
    }
}

fn stop_event_name(token: &str) -> io::Result<String> {
    let suffix: String = token
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    if suffix.is_empty() {
        return Err(io::Error::other("invalid broker ownership token"));
    }
    crate::windows_security::RuntimeIdentity::current()?.mutex_name(&format!("child-stop-{suffix}"))
}

/// 读取进程的创建时间，并将其编码为 64 位 FILETIME 值。
///
/// 此值用于在打开 Broker 进程后确认 PID 仍指向预期的进程实例。
///
/// # Errors
///
/// 系统无法读取进程时间时返回 Windows I/O 错误。
pub fn birth(handle: &OwnedHandle) -> io::Result<u64> {
    let (mut creation, mut exit, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    if !unsafe {
        GetProcessTimes(
            HANDLE(handle.as_raw_handle()),
            &mut creation,
            &mut exit,
            &mut kernel,
            &mut user,
        )
    }
    .as_bool()
    {
        return Err(io::Error::last_os_error());
    }
    Ok((creation.dwHighDateTime as u64) << 32 | creation.dwLowDateTime as u64)
}

/// 监视 Broker 生命周期，并在其退出时通知子服务关闭。
///
/// 该监视器通过一个工作线程等待本地取消事件、Broker 进程退出或 Broker 停止事件，
/// 不进行周期轮询。丢弃监视器会发出本地取消信号并等待工作线程结束；回调只在
/// Broker 进程或其停止事件先触发时运行。
pub struct ParentWatch {
    stop: OwnedHandle,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl ParentWatch {
    /// 启动 Broker 生命周期监视。
    ///
    /// 若当前进程没有 Broker 所有权信息，返回 `Ok(None)`；否则验证 Broker 的 PID、
    /// 创建时间和停止事件后启动等待线程。`on_exit` 在线程中执行，且仅当 Broker
    /// 进程退出或其停止事件置位时调用。
    ///
    /// # Errors
    ///
    /// 所有权信息不完整或无效、Broker 身份不匹配、事件/进程句柄无法打开，或工作线程
    /// 无法创建时返回错误。
    pub fn start(on_exit: impl FnOnce() + Send + 'static) -> io::Result<Option<Self>> {
        if INSTANCE.get().is_none() {
            let _ = INSTANCE.set(new_token()?);
        }
        let Some(owner) = LAUNCH_OWNER.get() else {
            return Ok(None);
        };
        let stop_name = stop_event_name(&owner.token)?;
        let wide = windows_strings::HSTRING::from(stop_name);
        let remote_stop = unsafe {
            OpenEventW(
                SYNCHRONIZE as u32,
                false,
                windows_core::PCWSTR(wide.as_ptr()),
            )
        };
        if remote_stop.0.is_null() {
            return Err(io::Error::last_os_error());
        }
        let remote_stop = unsafe { OwnedHandle::from_raw_handle(remote_stop.0) };
        let parent = open_process(owner.pid)?;
        if birth(&parent)? != owner.birth {
            return Err(io::Error::other("broker PID was reused"));
        }
        let stop = unsafe { CreateEventW(None, true, false, None) };
        if stop.0.is_null() {
            return Err(io::Error::last_os_error());
        }
        let stop = unsafe { OwnedHandle::from_raw_handle(stop.0) };
        let worker_stop = stop.try_clone()?;
        let worker = std::thread::Builder::new()
            .name("broker-lifetime".into())
            .spawn(move || {
                let handles = [
                    HANDLE(worker_stop.as_raw_handle()),
                    HANDLE(parent.as_raw_handle()),
                    HANDLE(remote_stop.as_raw_handle()),
                ];
                let result = unsafe { WaitForMultipleObjects(&handles, false, u32::MAX) };
                if result != 0 {
                    on_exit();
                }
            })?;
        Ok(Some(Self {
            stop,
            worker: Some(worker),
        }))
    }
}
impl Drop for ParentWatch {
    fn drop(&mut self) {
        unsafe {
            let _ = SetEvent(HANDLE(self.stop.as_raw_handle()));
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
