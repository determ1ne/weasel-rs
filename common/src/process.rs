//! Small process-lifetime primitives shared by the Windows executables.

use crate::bindings::{
    CloseHandle, CreateMutexW, ERROR_ALREADY_EXISTS, ERROR_INVALID_PARAMETER, GetLastError, HANDLE,
};
use crate::platform::{LocalSecurityDescriptor, RuntimeIdentity};
pub use crate::runtime_paths::executable_directory;
use windows_strings::HSTRING;

/// An owned named mutex. Its presence means this component is already running.
pub struct SingleInstance {
    handle: HANDLE,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SingleInstanceError {
    AlreadyRunning,
    CreateFailed(u32),
}

impl std::fmt::Display for SingleInstanceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyRunning => f.write_str("another instance is already running"),
            Self::CreateFailed(error) => write!(f, "CreateMutexW failed with Win32 error {error}"),
        }
    }
}

impl std::error::Error for SingleInstanceError {}

impl SingleInstance {
    pub fn acquire(component: &str) -> Result<Self, SingleInstanceError> {
        let identity = RuntimeIdentity::current().map_err(platform_error)?;
        Self::acquire_for(&identity, component)
    }

    pub fn acquire_for(
        identity: &RuntimeIdentity,
        component: &str,
    ) -> Result<Self, SingleInstanceError> {
        let name = identity.mutex_name(component).map_err(platform_error)?;
        let security = LocalSecurityDescriptor::for_logon(identity).map_err(platform_error)?;
        let attributes = security.security_attributes();
        let wide = HSTRING::from(name);
        // Presence only: no thread owns this mutex and no ReleaseMutex is needed.
        let handle = unsafe {
            CreateMutexW(
                Some(&attributes),
                false,
                windows_core::PCWSTR(wide.as_ptr()),
            )
        };
        if handle.0.is_null() {
            return Err(SingleInstanceError::CreateFailed(unsafe { GetLastError() }));
        }
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS as u32 {
            unsafe {
                let _ = CloseHandle(handle);
            }
            return Err(SingleInstanceError::AlreadyRunning);
        }
        Ok(Self { handle })
    }

    pub fn keep_alive(&self) {
        let _ = self.handle;
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

unsafe impl Send for SingleInstance {}
unsafe impl Sync for SingleInstance {}

fn platform_error(error: std::io::Error) -> SingleInstanceError {
    SingleInstanceError::CreateFailed(error.raw_os_error().unwrap_or(ERROR_INVALID_PARAMETER) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn presence_can_be_dropped_on_another_thread() {
        let component = format!("test-{}", std::process::id());
        let guard = SingleInstance::acquire(&component).unwrap();
        assert!(matches!(
            SingleInstance::acquire(&component),
            Err(SingleInstanceError::AlreadyRunning)
        ));
        std::thread::spawn(move || drop(guard)).join().unwrap();
        drop(SingleInstance::acquire(&component).unwrap());
    }
}
