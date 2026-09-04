//! Bounded component diagnostics. Never pass keystrokes, composition/candidate
//! text, surrounding text, committed input, or RPC payloads to this writer.
//! No global subscriber is installed; the application controls levels and fields.
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};

#[derive(Debug, Clone, Copy)]
pub struct Rotation {
    pub max_bytes: u64,
    pub retained_files: usize,
}
impl Default for Rotation {
    fn default() -> Self {
        Self {
            max_bytes: 2 * 1024 * 1024,
            retained_files: 3,
        }
    }
}

/// Clone a single writer per component/process. Independent writers/processes
/// must use distinct component names (rotation is synchronized within this writer).
#[derive(Clone)]
pub struct ComponentLogger(Arc<Mutex<Sink>>);
enum Sink {
    Stderr,
    File(RotatingFile),
}
struct RotatingFile {
    path: PathBuf,
    file: Option<File>,
    size: u64,
    rotation: Rotation,
}

impl ComponentLogger {
    pub fn stderr() -> Self {
        Self(Arc::new(Mutex::new(Sink::Stderr)))
    }
    pub fn file(directory: &Path, component: &str, rotation: Rotation) -> io::Result<Self> {
        crate::platform::validate_component(component)?;
        if rotation.max_bytes == 0 || rotation.retained_files > 100 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "rotation requires positive max_bytes and at most 100 retained files",
            ));
        }
        fs::create_dir_all(directory)?;
        let path = directory.join(format!("{component}.log"));
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let mut size = file.metadata()?.len();
        // Also bound an active file left behind by an older configuration.
        if size > rotation.max_bytes {
            // Windows append-only handles cannot change the end-of-file position.
            OpenOptions::new()
                .write(true)
                .open(&path)?
                .set_len(rotation.max_bytes)?;
            size = rotation.max_bytes;
        }
        let mut sink = RotatingFile {
            path,
            file: Some(file),
            size,
            rotation,
        };
        // Retained files from a larger previous limit must respect this limit too.
        for index in 1..=rotation.retained_files {
            match OpenOptions::new().write(true).open(sink.archive(index)) {
                Ok(file) if file.metadata()?.len() > rotation.max_bytes => {
                    file.set_len(rotation.max_bytes)?
                }
                Ok(_) => (),
                Err(error) if error.kind() == io::ErrorKind::NotFound => (),
                Err(error) => return Err(error),
            }
        }
        // Remove only this component's canonical numbered archives beyond retention.
        let prefix = format!("{component}.log.");
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let name = entry.file_name();
            if let Some(suffix) = name.to_str().and_then(|name| name.strip_prefix(&prefix))
                && let Ok(index) = suffix.parse::<usize>()
                && index > rotation.retained_files
                && suffix == index.to_string()
                && entry.file_type()?.is_file()
            {
                fs::remove_file(entry.path())?;
            }
        }
        if size == rotation.max_bytes {
            sink.rotate()?;
        }
        Ok(Self(Arc::new(Mutex::new(Sink::File(sink)))))
    }
    pub fn for_paths(
        paths: &crate::runtime_paths::RuntimePaths,
        component: &str,
    ) -> io::Result<Self> {
        Self::file(&paths.logs, component, Rotation::default())
    }
    /// Explicit stderr fallback; the returned error explains why file logging failed.
    pub fn file_or_stderr(
        directory: &Path,
        component: &str,
        rotation: Rotation,
    ) -> (Self, Option<io::Error>) {
        match Self::file(directory, component, rotation) {
            Ok(writer) => (writer, None),
            Err(error) => (Self::stderr(), Some(error)),
        }
    }
}

impl RotatingFile {
    fn archive(&self, index: usize) -> PathBuf {
        self.path.with_extension(format!("log.{index}"))
    }
    fn rotate(&mut self) -> io::Result<()> {
        if let Some(mut file) = self.file.take() {
            file.flush()?;
        }
        if self.rotation.retained_files == 0 {
            self.file = Some(File::create(&self.path)?);
        } else {
            for index in (1..=self.rotation.retained_files).rev() {
                let target = self.archive(index);
                match fs::remove_file(&target) {
                    Ok(()) => (),
                    Err(e) if e.kind() == io::ErrorKind::NotFound => (),
                    Err(e) => return Err(e),
                }
                let source = if index == 1 {
                    self.path.clone()
                } else {
                    self.archive(index - 1)
                };
                match fs::rename(source, target) {
                    Ok(()) => (),
                    Err(e) if e.kind() == io::ErrorKind::NotFound => (),
                    Err(e) => return Err(e),
                }
            }
            self.file = Some(File::create(&self.path)?);
        }
        self.size = 0;
        Ok(())
    }
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        if self.size >= self.rotation.max_bytes {
            self.rotate()?;
        }
        let capacity = (self.rotation.max_bytes - self.size).min(usize::MAX as u64) as usize;
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| io::Error::other("log rotation failed; reopen logger"))?;
        let count = file.write(&bytes[..bytes.len().min(capacity)])?;
        self.size += count as u64;
        Ok(count)
    }
}

impl Write for ComponentLogger {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut sink = self
            .0
            .lock()
            .map_err(|_| io::Error::other("log lock poisoned"))?;
        match &mut *sink {
            Sink::Stderr => io::stderr().write(bytes),
            Sink::File(file) => file.write(bytes),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        let mut sink = self
            .0
            .lock()
            .map_err(|_| io::Error::other("log lock poisoned"))?;
        match &mut *sink {
            Sink::Stderr => io::stderr().flush(),
            Sink::File(file) => file
                .file
                .as_mut()
                .ok_or_else(|| io::Error::other("log file unavailable"))?
                .flush(),
        }
    }
}
impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for ComponentLogger {
    type Writer = ComponentWriter<'a>;
    fn make_writer(&'a self) -> Self::Writer {
        ComponentWriter(self.0.lock().ok())
    }
}

/// Holds the component lock for a complete tracing event, including oversized writes.
pub struct ComponentWriter<'a>(Option<MutexGuard<'a, Sink>>);
impl Write for ComponentWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match self
            .0
            .as_deref_mut()
            .ok_or_else(|| io::Error::other("log lock poisoned"))?
        {
            Sink::Stderr => io::stderr().write(bytes),
            Sink::File(file) => file.write(bytes),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self
            .0
            .as_deref_mut()
            .ok_or_else(|| io::Error::other("log lock poisoned"))?
        {
            Sink::Stderr => io::stderr().flush(),
            Sink::File(file) => file
                .file
                .as_mut()
                .ok_or_else(|| io::Error::other("log file unavailable"))?
                .flush(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn oversized_writes_remain_bounded_and_keep_latest_bytes() {
        let directory = std::env::temp_dir().join(format!(
            "weasel-log-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut writer = ComponentLogger::file(
            &directory,
            "test",
            Rotation {
                max_bytes: 4,
                retained_files: 2,
            },
        )
        .unwrap();
        writer.write_all(b"abcdefghijklmnop").unwrap();
        writer.flush().unwrap();
        assert_eq!(fs::read(directory.join("test.log")).unwrap(), b"mnop");
        assert_eq!(fs::read(directory.join("test.log.1")).unwrap(), b"ijkl");
        assert_eq!(fs::read(directory.join("test.log.2")).unwrap(), b"efgh");
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 3);
        drop(writer);
        // Reopening with smaller size/retention also bounds pre-existing files.
        let mut writer = ComponentLogger::file(
            &directory,
            "test",
            Rotation {
                max_bytes: 2,
                retained_files: 1,
            },
        )
        .unwrap();
        {
            use tracing_subscriber::fmt::MakeWriter;
            let mut event = writer.make_writer();
            event.write_all(b"uvwxyz").unwrap();
            event.flush().unwrap();
        }
        writer.flush().unwrap();
        assert_eq!(fs::read(directory.join("test.log")).unwrap(), b"yz");
        assert_eq!(fs::read(directory.join("test.log.1")).unwrap(), b"wx");
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 2);
        drop(writer);
        for file in fs::read_dir(&directory).unwrap() {
            fs::remove_file(file.unwrap().path()).unwrap();
        }
        fs::remove_dir(directory).unwrap();
    }
    #[test]
    fn rejects_bad_configuration_and_component() {
        assert!(ComponentLogger::file(Path::new("."), "../escape", Rotation::default()).is_err());
        assert!(
            ComponentLogger::file(
                Path::new("."),
                "test",
                Rotation {
                    max_bytes: 0,
                    retained_files: 0
                }
            )
            .is_err()
        );
    }
}
