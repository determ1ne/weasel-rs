//! Component-local tracing subscriber; does not affect host or other DLL subscribers.
use std::{fmt, sync::OnceLock};
use weasel_common::{
    logging::{ComponentLogger, Level},
    runtime_paths::RuntimePaths,
};
static LOGGER: OnceLock<ComponentLogger> = OnceLock::new();
pub fn initialize() {
    let (logger, error) = match RuntimePaths::discover() {
        Ok(paths) => ComponentLogger::file_or_stderr(&paths.logs, "renderer"),
        Err(error) => (ComponentLogger::stderr(), Some(error)),
    };
    let _ = LOGGER.set(logger);
    if let Some(error) = error {
        LOGGER.get_or_init(ComponentLogger::stderr).record(
            Level::WARN,
            "weasel-renderer",
            format_args!("file logging unavailable; using stderr: {error}"),
        );
    }
}
pub fn record(message: fmt::Arguments<'_>) {
    record_at(Level::INFO, message);
}
pub fn record_at(level: Level, message: fmt::Arguments<'_>) {
    LOGGER
        .get_or_init(ComponentLogger::stderr)
        .record(level, "weasel-renderer", message);
}
