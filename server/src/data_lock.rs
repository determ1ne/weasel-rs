//! 通过数据目录中的文件句柄独占 Rime 用户数据目录。
//!
//! Windows 以禁止共享的方式打开固定锁文件，因此不同安装实例及登录会话也会互斥。锁的
//! 有效期与句柄生命周期一致；关闭句柄释放互斥，但锁文件本身保留在数据目录中。
use std::{
    fs::{File, OpenOptions},
    os::windows::fs::OpenOptionsExt,
    path::Path,
};

/// 持有用户数据目录的独占访问权。
///
/// 文件句柄必须保持打开才能维持锁；该类型析构时由操作系统释放句柄和独占状态。
pub(crate) struct DataLock {
    /// 锁文件句柄；字段不对外暴露，避免调用方提前关闭锁。
    _file: File,
}
impl DataLock {
    /// 打开或创建用户数据目录中的 `.weasel.lock` 并取得独占句柄。
    ///
    /// 不截断已有文件，也不创建数据目录。文件被其他进程占用或路径不可用时返回带原因
    /// 的错误；成功返回的锁持续到 `DataLock` 被销毁。
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
