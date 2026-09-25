//! 组件日志的时间格式、输出端及轻量日志句柄。
//!
//! 文件创建、轮转和保留策略由 `tracing-appender` 管理。本模块不会安装进程级
//! subscriber；各组件可创建自己的 [`ComponentLogger`]，也可自行配置 tracing。
use std::{
    io::{self, Write},
    path::Path,
    sync::Arc,
};

/// tracing 使用的日志等级类型。
pub use tracing::Level;
use tracing_subscriber::fmt::{MakeWriter, writer::BoxMakeWriter};

/// 将系统时间格式化为 UTC 时间戳，固定保留 6 位微秒小数。
///
/// 返回形式为 `YYYY-MM-DDTHH:MM:SS.ffffffZ`，用于当前日志以及按事件时间记录的
/// 历史 TIP 事件。超出日期库可表示范围时返回 `invalid-timestamp`。
pub fn timestamp(value: std::time::SystemTime) -> String {
    let nanos = match value.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_nanos() as i128,
        Err(e) => -(e.duration().as_nanos() as i128),
    };
    let Ok(t) = time::OffsetDateTime::from_unix_timestamp_nanos(nanos) else {
        return "invalid-timestamp".into();
    };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:06}Z",
        t.year(),
        t.month() as u8,
        t.day(),
        t.hour(),
        t.minute(),
        t.second(),
        t.microsecond()
    )
}

/// 为 tracing-subscriber 提供统一 UTC 微秒时间戳的格式化器。
pub struct UtcTimer;
impl tracing_subscriber::fmt::time::FormatTime for UtcTimer {
    fn format_time(
        &self,
        writer: &mut tracing_subscriber::fmt::format::Writer<'_>,
    ) -> std::fmt::Result {
        write!(writer, "{}", timestamp(std::time::SystemTime::now()))
    }
}

#[derive(Clone)]
struct Writer(Arc<BoxMakeWriter>);
impl<'a> MakeWriter<'a> for Writer {
    type Writer = Box<dyn Write + 'a>;
    fn make_writer(&'a self) -> Self::Writer {
        self.0.make_writer()
    }
}

/// 可克隆的组件日志句柄，可用于局部 tracing subscriber 或直接写入输出。
///
/// 句柄共享底层 writer，并持有独立的 tracing dispatch；创建句柄不会启动后台线程，
/// 也不要求安装进程级 subscriber 或额外执行全局关闭流程。
#[derive(Clone)]
pub struct ComponentLogger {
    writer: Writer,
    dispatch: tracing::Dispatch,
}
impl ComponentLogger {
    fn new(writer: BoxMakeWriter) -> Self {
        let writer = Writer(Arc::new(writer));
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_timer(UtcTimer)
            .with_max_level(Level::TRACE)
            .with_writer(writer.clone())
            .finish();
        Self {
            writer,
            dispatch: tracing::Dispatch::new(subscriber),
        }
    }
    /// 创建将日志写到标准错误的组件 logger。
    pub fn stderr() -> Self {
        Self::new(BoxMakeWriter::new(io::stderr))
    }
    /// 在指定目录创建按日轮转的文件 logger。
    ///
    /// `component` 用作文件名前缀，必须是由 ASCII 字母、数字、连字符或下划线组成的
    /// 非空组件标识，且长度不超过 64 字节。最多保留 4 个日志文件。目录不可写或名称
    /// 不合法时返回错误。
    pub fn file(directory: &Path, component: &str) -> io::Result<Self> {
        crate::windows_security::validate_component(component)?;
        let appender = tracing_appender::rolling::Builder::new()
            .rotation(tracing_appender::rolling::Rotation::DAILY)
            .filename_prefix(component)
            .filename_suffix("log")
            .max_log_files(4)
            .build(directory)
            .map_err(io::Error::other)?;
        Ok(Self::new(BoxMakeWriter::new(appender)))
    }
    /// 使用运行时路径中的日志目录创建文件 logger。
    pub fn for_paths(paths: &crate::process::RuntimePaths, component: &str) -> io::Result<Self> {
        Self::file(&paths.logs, component)
    }
    /// 优先创建文件 logger，失败时回退到标准错误输出。
    ///
    /// 返回的错误仅在回退发生时为 `Some`，调用方可据此记录日志目录不可用的原因。
    pub fn file_or_stderr(directory: &Path, component: &str) -> (Self, Option<io::Error>) {
        match Self::file(directory, component) {
            Ok(logger) => (logger, None),
            Err(error) => (Self::stderr(), Some(error)),
        }
    }
    /// 以指定等级和组件字段记录一条格式化日志。
    ///
    /// `message` 只在本次调用期间借用；日志通过此句柄自己的 dispatch 发出，不依赖
    /// 当前线程安装的默认 subscriber。
    pub fn record(&self, level: Level, component: &str, message: std::fmt::Arguments<'_>) {
        tracing::dispatcher::with_default(&self.dispatch, || match level {
            Level::ERROR => tracing::error!(component, "{message}"),
            Level::WARN => tracing::warn!(component, "{message}"),
            Level::INFO => tracing::info!(component, "{message}"),
            Level::DEBUG => tracing::debug!(component, "{message}"),
            Level::TRACE => tracing::trace!(component, "{message}"),
        });
    }
}
impl<'a> MakeWriter<'a> for ComponentLogger {
    type Writer = Box<dyn Write + 'a>;
    fn make_writer(&'a self) -> Self::Writer {
        self.writer.make_writer()
    }
}
impl Write for ComponentLogger {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.make_writer().write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.make_writer().flush()
    }
}
