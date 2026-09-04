//! Runtime layout is selected by a marker next to the executable, not the CWD
//! or Cargo's debug/release profile.
use std::{
    io,
    path::{Path, PathBuf},
};

pub fn executable_directory() -> io::Result<PathBuf> {
    std::env::current_exe()?
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| io::Error::other("executable has no parent directory"))
}

pub fn is_development_directory(directory: &Path) -> bool {
    directory.join(".dev").is_file()
}

pub fn is_development() -> bool {
    executable_directory().is_ok_and(|directory| is_development_directory(&directory))
}

/// Shared executable-relative development and per-user installed layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimePaths {
    pub executable_directory: PathBuf,
    pub development: bool,
    pub user_data: PathBuf,
    pub logs: PathBuf,
}

impl RuntimePaths {
    pub fn discover() -> io::Result<Self> {
        Self::for_directory(executable_directory()?)
    }

    pub fn for_directory(directory: PathBuf) -> io::Result<Self> {
        let development = is_development_directory(&directory);
        Self::select(
            directory,
            development,
            std::env::var_os("APPDATA").map(PathBuf::from),
            std::env::var_os("LOCALAPPDATA").map(PathBuf::from),
        )
    }

    /// Pure selection: does not read environment variables or create directories.
    pub fn select(
        directory: PathBuf,
        development: bool,
        appdata: Option<PathBuf>,
        local_appdata: Option<PathBuf>,
    ) -> io::Result<Self> {
        let user_data = select_user_data(&directory, development, appdata)?;
        let logs = if development {
            directory.join("logs")
        } else {
            local_appdata
                .filter(|p| p.is_absolute())
                .ok_or_else(|| {
                    io::Error::other("LOCALAPPDATA must be set to an absolute directory")
                })?
                .join("Weasel-RS")
                .join("Logs")
        };
        Ok(Self {
            executable_directory: directory,
            development,
            user_data,
            logs,
        })
    }

    pub fn ensure(&self) -> io::Result<()> {
        std::fs::create_dir_all(&self.user_data)?;
        std::fs::create_dir_all(&self.logs)
    }
}

fn select_user_data(
    directory: &Path,
    development: bool,
    appdata: Option<PathBuf>,
) -> io::Result<PathBuf> {
    if development {
        return Ok(directory.join("user-data"));
    }
    let appdata = appdata
        .filter(|path| path.is_absolute())
        .ok_or_else(|| io::Error::other("APPDATA must be set to an absolute directory"))?;
    Ok(appdata.join("Weasel-RS").join("Rime"))
}

/// Both ordinary service initialization and deployment must call this.
pub fn ensure_user_data_directory(directory: &Path) -> io::Result<PathBuf> {
    let path = select_user_data(
        directory,
        is_development_directory(directory),
        std::env::var_os("APPDATA").map(PathBuf::from),
    )?;
    std::fs::create_dir_all(&path)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_logs_without_touching_user_directories() {
        let base = std::env::temp_dir().join("weasel-path-selection-only");
        let dev = RuntimePaths::select(base.clone(), true, None, None).unwrap();
        assert_eq!(dev.logs, base.join("logs"));
        assert_eq!(dev.user_data, base.join("user-data"));
        let installed = RuntimePaths::select(
            base.clone(),
            false,
            Some(base.join("roaming")),
            Some(base.join("local")),
        )
        .unwrap();
        assert_eq!(installed.logs, base.join("local/Weasel-RS/Logs"));
        assert!(RuntimePaths::select(base.clone(), false, Some(base.clone()), None).is_err());
        assert!(
            RuntimePaths::select(base.clone(), false, Some(base), Some("relative".into())).is_err()
        );
    }

    #[test]
    fn development_uses_local_data_even_without_appdata() {
        let base = Path::new(r"C:\Weasel");
        assert_eq!(
            select_user_data(base, true, None).unwrap(),
            base.join("user-data")
        );
    }

    #[test]
    fn installed_data_uses_roaming_directory() {
        let roaming = PathBuf::from(r"C:\Users\测试\AppData\Roaming");
        assert_eq!(
            select_user_data(Path::new(r"C:\Weasel"), false, Some(roaming.clone())).unwrap(),
            roaming.join("Weasel-RS").join("Rime")
        );
    }

    #[test]
    fn installed_mode_does_not_fall_back_to_install_directory() {
        assert!(select_user_data(Path::new("."), false, None).is_err());
        assert!(select_user_data(Path::new("."), false, Some(PathBuf::from("relative"))).is_err());
    }
}
