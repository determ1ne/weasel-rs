//! Process diagnostics only: never log snapshots, candidates, or input text.
use std::{
    fmt,
    io::Write,
    sync::{Mutex, OnceLock},
};
use weasel_common::{
    logging::{ComponentLogger, Rotation},
    runtime_paths::RuntimePaths,
};

static LOGGER: OnceLock<Mutex<ComponentLogger>> = OnceLock::new();

fn for_paths(
    paths: &RuntimePaths,
    rotation: Rotation,
) -> (ComponentLogger, Option<std::io::Error>) {
    ComponentLogger::file_or_stderr(&paths.logs, "renderer", rotation)
}

/// Called after acquiring the singleton so only one process rotates renderer.log.
pub fn initialize() {
    let (logger, error) = match RuntimePaths::discover() {
        Ok(paths) => for_paths(&paths, Rotation::default()),
        Err(error) => (ComponentLogger::stderr(), Some(error)),
    };
    let _ = LOGGER.set(Mutex::new(logger));
    if let Some(error) = error {
        record(format_args!(
            "file logging unavailable; using stderr: {error}"
        ));
    }
}

pub fn record(message: fmt::Arguments<'_>) {
    if let Some(logger) = LOGGER.get() {
        if let Ok(mut logger) = logger.lock() {
            if writeln!(logger, "weasel-renderer: {message}")
                .and_then(|_| logger.flush())
                .is_ok()
            {
                return;
            }
        }
    }
    eprintln!("weasel-renderer: {message}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installed_logs_rotate_under_local_appdata_only() {
        let directory = std::env::temp_dir().join(format!(
            "renderer-logs-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let install = directory.join("install");
        let paths = RuntimePaths::select(
            install.clone(),
            false,
            Some(directory.join("roaming")),
            Some(directory.join("local")),
        )
        .unwrap();
        let (mut logger, error) = for_paths(
            &paths,
            Rotation {
                max_bytes: 4,
                retained_files: 2,
            },
        );
        assert!(error.is_none());
        logger.write_all(b"abcdefghijklmnop").unwrap();
        logger.flush().unwrap();
        drop(logger);
        assert_eq!(paths.logs, directory.join("local/Weasel-RS/Logs"));
        assert_eq!(
            std::fs::read(paths.logs.join("renderer.log")).unwrap(),
            b"mnop"
        );
        assert_eq!(
            std::fs::read(paths.logs.join("renderer.log.1")).unwrap(),
            b"ijkl"
        );
        assert_eq!(
            std::fs::read(paths.logs.join("renderer.log.2")).unwrap(),
            b"efgh"
        );
        assert_eq!(std::fs::read_dir(&paths.logs).unwrap().count(), 3);
        assert!(!install.exists());
        assert!(!paths.user_data.exists());
        // Remove only the files/directories this test created.
        for name in ["renderer.log", "renderer.log.1", "renderer.log.2"] {
            std::fs::remove_file(paths.logs.join(name)).unwrap();
        }
        for path in [
            &paths.logs,
            &directory.join("local/Weasel-RS"),
            &directory.join("local"),
            &directory,
        ] {
            std::fs::remove_dir(path).unwrap();
        }
    }
}
