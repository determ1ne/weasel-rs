//! Windows 可执行组件共用的进程路径与生命周期原语。

use std::{
    io,
    path::{Path, PathBuf},
};

use crate::bindings::{
    CloseHandle, CreateMutexW, ERROR_ALREADY_EXISTS, ERROR_INVALID_PARAMETER, GetLastError, HANDLE,
};
use crate::windows_security::{LocalSecurityDescriptor, RuntimeIdentity};
use windows_strings::HSTRING;

/// 返回当前进程映像所在目录。
///
/// 结果来自操作系统提供的当前可执行文件路径，不受进程当前工作目录影响。
///
/// # Errors
///
/// 无法取得当前可执行文件路径，或该路径没有父目录时返回错误。
pub fn executable_directory() -> io::Result<PathBuf> {
    std::env::current_exe()?
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| io::Error::other("executable has no parent directory"))
}

/// 当前进程使用的程序、用户数据和日志目录。
///
/// 用户数据位于漫游配置目录，日志位于本地应用数据目录。此结构只保存路径，
/// 不代表目录已经存在；需要目录时应调用 [`RuntimePaths::ensure`]。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimePaths {
    /// 当前组件可执行文件所在目录。
    pub executable_directory: PathBuf,
    /// Rime 用户数据目录。
    pub user_data: PathBuf,
    /// 组件日志目录。
    pub logs: PathBuf,
}

impl RuntimePaths {
    /// 根据当前可执行文件位置和进程环境解析运行时目录。
    ///
    /// 本方法只计算目录，不创建目录，并要求 `APPDATA` 和 `LOCALAPPDATA`
    /// 环境变量提供绝对路径。
    ///
    /// # Errors
    ///
    /// 无法定位可执行文件，或缺少有效的用户目录环境变量时返回错误。
    pub fn discover() -> io::Result<Self> {
        Self::select(
            executable_directory()?,
            std::env::var_os("APPDATA").map(PathBuf::from),
            std::env::var_os("LOCALAPPDATA").map(PathBuf::from),
        )
    }

    /// 根据明确提供的输入纯计算目录布局。
    fn select(
        executable_directory: PathBuf,
        appdata: Option<PathBuf>,
        local_appdata: Option<PathBuf>,
    ) -> io::Result<Self> {
        let user_data = appdata
            .filter(|path| path.is_absolute())
            .ok_or_else(|| io::Error::other("APPDATA must be set to an absolute directory"))?
            .join("Weasel-RS")
            .join("Rime");
        let logs = local_appdata
            .filter(|path| path.is_absolute())
            .ok_or_else(|| io::Error::other("LOCALAPPDATA must be set to an absolute directory"))?
            .join("Weasel-RS")
            .join("Logs");
        Ok(Self {
            executable_directory,
            user_data,
            logs,
        })
    }

    /// 创建用户数据目录和日志目录（若它们尚不存在）。
    ///
    /// # Errors
    ///
    /// 创建任一目录失败时返回底层 I/O 错误。
    pub fn ensure(&self) -> io::Result<()> {
        std::fs::create_dir_all(&self.user_data)?;
        std::fs::create_dir_all(&self.logs)
    }
}

/// 表示当前组件持有单实例互斥体的 RAII 守卫。
///
/// 该类型拥有底层 Win32 句柄，并在丢弃时关闭句柄。互斥体只用于检测对象是否
/// 已存在，不由任何线程取得所有权，因此释放时无需调用 `ReleaseMutex`。
pub struct SingleInstance {
    handle: HANDLE,
}

/// 获取单实例互斥体时可能发生的错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SingleInstanceError {
    /// 同一用户登录会话中已有该组件实例。
    AlreadyRunning,
    /// 创建互斥体或准备其安全属性失败，包含对应的 Win32 错误码。
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
    /// 根据当前进程的 Windows 用户和登录会话身份获取组件互斥体。
    ///
    /// `component` 会成为内核对象名称的一部分，必须符合
    /// [`RuntimeIdentity::mutex_name`] 的命名约束。
    ///
    /// # Errors
    ///
    /// 若无法读取当前进程身份、构造安全描述符或创建互斥体，则返回
    /// [`SingleInstanceError::CreateFailed`]；若对象已存在，则返回
    /// [`SingleInstanceError::AlreadyRunning`]。
    pub fn acquire(component: &str) -> Result<Self, SingleInstanceError> {
        let identity = RuntimeIdentity::current().map_err(platform_error)?;
        Self::acquire_for(&identity, component)
    }

    /// 使用给定 Windows 身份获取组件互斥体。
    ///
    /// 调用方负责确保 `identity` 对应当前进程的预期用户和登录会话。返回的
    /// 守卫拥有互斥体句柄；守卫存活期间，同名实例无法再次成功获取。
    ///
    /// # Errors
    ///
    /// 对象名称无效、安全描述符创建失败或 Win32 互斥体创建失败时返回
    /// [`SingleInstanceError::CreateFailed`]；同名对象已存在时返回
    /// [`SingleInstanceError::AlreadyRunning`]。
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
mod path_tests {
    use super::*;

    #[test]
    fn selects_installed_paths_without_touching_directories() {
        let base = std::env::temp_dir().join("weasel-path-selection-only");
        let installed = RuntimePaths::select(
            base.clone(),
            Some(base.join("roaming")),
            Some(base.join("local")),
        )
        .unwrap();
        assert_eq!(installed.user_data, base.join("roaming/Weasel-RS/Rime"));
        assert_eq!(installed.logs, base.join("local/Weasel-RS/Logs"));
        assert!(RuntimePaths::select(base.clone(), Some(base.clone()), None).is_err());
        assert!(RuntimePaths::select(base.clone(), Some(base), Some("relative".into())).is_err());
    }

    #[test]
    fn installed_data_uses_roaming_directory() {
        let roaming = PathBuf::from(r"C:\Users\测试\AppData\Roaming");
        let local = PathBuf::from(r"C:\Users\测试\AppData\Local");
        assert_eq!(
            RuntimePaths::select(
                PathBuf::from(r"C:\Program Files\Weasel-RS"),
                Some(roaming.clone()),
                Some(local),
            )
            .unwrap()
            .user_data,
            roaming.join("Weasel-RS").join("Rime")
        );
    }

    #[test]
    fn user_data_does_not_fall_back_to_install_directory() {
        let directory = PathBuf::from(r"C:\Program Files\Weasel-RS");
        let local = Some(PathBuf::from(r"C:\Users\测试\AppData\Local"));
        assert!(RuntimePaths::select(directory.clone(), None, local.clone()).is_err());
        assert!(RuntimePaths::select(directory, Some("relative".into()), local).is_err());
    }
}
