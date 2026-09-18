//! Ownership is a control-plane handshake, never a replacement for pipe ACLs.
use crate::bindings::*;
use std::{
    io,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    sync::OnceLock,
};

pub const TOKEN_ENV: &str = "WEASEL_BROKER_TOKEN";
pub const PID_ENV: &str = "WEASEL_BROKER_PID";
pub const BIRTH_ENV: &str = "WEASEL_BROKER_BIRTH";
pub const STOP_ENV: &str = "WEASEL_BROKER_STOP_EVENT";
static INSTANCE: OnceLock<String> = OnceLock::new();
static LAUNCH_OWNER: OnceLock<[String; 4]> = OnceLock::new();

/// Shell launches cannot carry Command::env overrides. Consume the private
/// ownership prefix without mutating the process-wide environment.
pub fn take_launch_args(
    args: impl IntoIterator<Item = std::ffi::OsString>,
) -> io::Result<Vec<std::ffi::OsString>> {
    let mut args = args.into_iter().peekable();
    if args.peek().is_some_and(|arg| arg == "--broker-owner") {
        args.next();
        let mut values = std::array::from_fn::<_, 4, _>(|_| String::new());
        for value in &mut values {
            *value = args
                .next()
                .and_then(|arg| arg.into_string().ok())
                .filter(|arg| !arg.is_empty())
                .ok_or_else(|| io::Error::other("incomplete broker ownership arguments"))?;
        }
        values[1].parse::<u32>().map_err(io::Error::other)?;
        values[2].parse::<u64>().map_err(io::Error::other)?;
        LAUNCH_OWNER
            .set(values)
            .map_err(|_| io::Error::other("broker ownership already initialized"))?;
    }
    Ok(args.collect())
}

fn owner_value(index: usize, env: &str) -> Option<String> {
    LAUNCH_OWNER
        .get()
        .map(|values| values[index].clone())
        .or_else(|| std::env::var(env).ok())
}

pub fn new_token() -> io::Result<String> {
    unsafe {
        CoCreateGuid()
            .map(|v| format!("{v:?}"))
            .map_err(io::Error::other)
    }
}

pub fn identity(component: &str, ready: bool) -> crate::message::ServiceIdentity {
    crate::message::ServiceIdentity {
        pid: std::process::id(),
        component: component.into(),
        ready,
        owner_token: owner_value(0, TOKEN_ENV).unwrap_or_default(),
        instance: INSTANCE.get().cloned().unwrap_or_default(),
    }
}

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

/// Dropping ownership requests graceful exit, including failed startup rollback.
pub struct StopSignal {
    handle: OwnedHandle,
    pub name: String,
}
impl StopSignal {
    pub fn new() -> io::Result<Self> {
        let identity = crate::platform::RuntimeIdentity::current()?;
        let suffix: String = new_token()?
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect();
        let name = identity.mutex_name(&format!("child-stop-{suffix}"))?;
        let security = crate::platform::LocalSecurityDescriptor::for_logon(&identity)?;
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
            name,
        })
    }
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

/// One event-driven wait thread, cancelled when the service exits normally.
/// Parent death wakes the service's ordinary graceful-shutdown path.
pub struct ParentWatch {
    stop: OwnedHandle,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl ParentWatch {
    pub fn start(on_exit: impl FnOnce() + Send + 'static) -> io::Result<Option<Self>> {
        if INSTANCE.get().is_none() {
            let _ = INSTANCE.set(new_token()?);
        }
        let token = owner_value(0, TOKEN_ENV);
        let pid = owner_value(1, PID_ENV);
        let expected = owner_value(2, BIRTH_ENV);
        if token.is_none() && pid.is_none() && expected.is_none() {
            return Ok(None);
        }
        if token.as_ref().is_none_or(|v| v.is_empty()) {
            return Err(io::Error::other("missing broker token"));
        }
        let pid: u32 = pid
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| io::Error::other("invalid broker PID"))?;
        let expected: u64 = expected
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| io::Error::other("invalid broker creation time"))?;
        let stop_name = owner_value(3, STOP_ENV)
            .ok_or_else(|| io::Error::other("missing broker stop event"))?;
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
        let parent = open_process(pid)?;
        if birth(&parent)? != expected {
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
