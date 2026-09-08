//! Shared tracing configuration. File creation, retention and rotation belong to
//! tracing-appender. No global subscriber is installed by this library.
use std::{
    io::{self, Write},
    path::Path,
    sync::Arc,
};
pub use tracing::Level;
use tracing_subscriber::fmt::{MakeWriter, writer::BoxMakeWriter};

/// UTC RFC 3339 with fixed microsecond precision, including historical TIP events.
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

/// Compatibility handle for component-local subscribers and raw deployment output.
/// No background thread: safe to use without a process-global shutdown guard.
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
    pub fn stderr() -> Self {
        Self::new(BoxMakeWriter::new(io::stderr))
    }
    pub fn file(directory: &Path, component: &str) -> io::Result<Self> {
        crate::platform::validate_component(component)?;
        let appender = tracing_appender::rolling::Builder::new()
            .rotation(tracing_appender::rolling::Rotation::DAILY)
            .filename_prefix(component)
            .filename_suffix("log")
            .max_log_files(4)
            .build(directory)
            .map_err(io::Error::other)?;
        Ok(Self::new(BoxMakeWriter::new(appender)))
    }
    pub fn for_paths(
        paths: &crate::runtime_paths::RuntimePaths,
        component: &str,
    ) -> io::Result<Self> {
        Self::file(&paths.logs, component)
    }
    pub fn file_or_stderr(directory: &Path, component: &str) -> (Self, Option<io::Error>) {
        match Self::file(directory, component) {
            Ok(logger) => (logger, None),
            Err(error) => (Self::stderr(), Some(error)),
        }
    }
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
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn timestamp_is_utc_with_six_fractional_digits() {
        let t = std::time::UNIX_EPOCH + std::time::Duration::new(0, 760_497_999);
        assert_eq!(timestamp(t), "1970-01-01T00:00:00.760497Z");
    }
}
