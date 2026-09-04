//! Exclusive per-data-directory ownership, shared across installations and logon sessions.
use std::{
    fs::{File, OpenOptions},
    os::windows::fs::OpenOptionsExt,
    path::Path,
};

pub(crate) struct DataLock {
    _file: File,
}
impl DataLock {
    pub fn acquire(user_data: &Path) -> Result<Self, String> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(0)
            .open(user_data.join(".weasel.lock"))
            .map_err(|error| format!("Rime user data is unavailable or in use: {error}"))?;
        Ok(Self { _file: file })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn data_directory_is_exclusive_until_owner_drops() {
        let directory = std::env::temp_dir().join(format!(
            "weasel-lock-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&directory).unwrap();
        let first = DataLock::acquire(&directory).unwrap();
        assert!(DataLock::acquire(&directory).is_err());
        drop(first);
        let second = DataLock::acquire(&directory).unwrap();
        drop(second);
        let lock = directory.join(".weasel.lock");
        assert_eq!(std::fs::metadata(&lock).unwrap().len(), 0);
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);
        std::fs::remove_file(lock).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }
}
