//! Shell activation is required for a UIAccess renderer. Keep its real process
//! handle so ownership checks and shutdown supervision remain unchanged.
use crate::bindings::*;
use std::os::windows::{
    io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle},
    process::ExitStatusExt,
};
use std::{io, path::PathBuf, process::ExitStatus};
use windows_strings::{HSTRING, w};

pub enum ChildProcess {
    Direct(std::process::Child),
    Shell { handle: OwnedHandle, pid: u32 },
}

impl ChildProcess {
    pub fn shell(file: PathBuf, directory: PathBuf, args: Vec<String>) -> io::Result<Self> {
        // Startup and restart can run on different apartments. Use a short-lived
        // STA and synchronous Shell activation; never change broker's apartment.
        std::thread::Builder::new()
            .name("renderer-launch".into())
            .spawn(move || unsafe {
                CoInitializeEx(None, COINIT_APARTMENTTHREADED as u32)
                    .ok()
                    .map_err(io::Error::other)?;
                struct Apartment;
                impl Drop for Apartment {
                    fn drop(&mut self) {
                        unsafe {
                            CoUninitialize();
                        }
                    }
                }
                let _apartment = Apartment;
                let file = HSTRING::from(file.as_os_str());
                let directory = HSTRING::from(directory.as_os_str());
                let parameters = HSTRING::from(
                    args.iter()
                        .map(|arg| quote(arg))
                        .collect::<Vec<_>>()
                        .join(" "),
                );
                let mut info = SHELLEXECUTEINFOW {
                    cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
                    fMask: (SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI)
                        as u32,
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
                Ok(Self::Shell { handle, pid })
            })?
            .join()
            .map_err(|_| io::Error::other("renderer launch thread panicked"))?
    }

    pub fn id(&self) -> u32 {
        match self {
            Self::Direct(child) => child.id(),
            Self::Shell { pid, .. } => *pid,
        }
    }

    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        match self {
            Self::Direct(child) => child.try_wait(),
            Self::Shell { handle, .. } => wait(handle, 0),
        }
    }

    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        match self {
            Self::Direct(child) => child.wait(),
            Self::Shell { handle, .. } => wait(handle, u32::MAX)?
                .ok_or_else(|| io::Error::other("unexpected process wait timeout")),
        }
    }
}

impl AsRawHandle for ChildProcess {
    fn as_raw_handle(&self) -> RawHandle {
        match self {
            Self::Direct(child) => child.as_raw_handle(),
            Self::Shell { handle, .. } => handle.as_raw_handle(),
        }
    }
}

fn wait(handle: &OwnedHandle, timeout: u32) -> io::Result<Option<ExitStatus>> {
    unsafe {
        match WaitForSingleObject(HANDLE(handle.as_raw_handle()), timeout) {
            0 => {
                let mut code = 0;
                if !GetExitCodeProcess(HANDLE(handle.as_raw_handle()), &mut code).as_bool() {
                    return Err(io::Error::last_os_error());
                }
                Ok(Some(ExitStatus::from_raw(code)))
            }
            258 => Ok(None),
            _ => Err(io::Error::last_os_error()),
        }
    }
}

// Windows argv quoting, including backslashes preceding quotes/end-of-string.
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

#[cfg(test)]
mod tests {
    #[test]
    fn quotes_shell_arguments() {
        assert_eq!(super::quote(""), "\"\"");
        assert_eq!(super::quote("a b"), "\"a b\"");
        assert_eq!(super::quote("a\"b"), "\"a\\\"b\"");
        assert_eq!(super::quote("Local\\event\\"), "\"Local\\event\\\\\"");
    }
}
