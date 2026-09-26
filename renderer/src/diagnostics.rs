//! 提供仅属于 renderer 组件的日志订阅与记录入口。
//!
//! 日志器在进程内只初始化一次，优先写入运行时日志目录；路径发现或文件日志
//! 不可用时使用标准错误输出。此订阅器不会替换宿主进程或其他 DLL 的日志订阅器。
use std::{fmt, sync::OnceLock};
use weasel_common::{
    logging::{ComponentLogger, Level},
    process::RuntimePaths,
};
static LOGGER: OnceLock<ComponentLogger> = OnceLock::new();
/// 初始化 renderer 日志器；重复调用不会替换已安装的实例。
///
/// 路径发现失败会保留 stderr 日志器，并记录降级原因，保证初始化错误仍可见。
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
/// 以 INFO 级别记录 renderer 诊断信息。
pub fn record(message: fmt::Arguments<'_>) {
    record_at(Level::INFO, message);
}
/// 使用指定级别记录信息；尚未显式初始化时惰性启用 stderr 日志器。
pub fn record_at(level: Level, message: fmt::Arguments<'_>) {
    LOGGER
        .get_or_init(ComponentLogger::stderr)
        .record(level, "weasel-renderer", message);
}
