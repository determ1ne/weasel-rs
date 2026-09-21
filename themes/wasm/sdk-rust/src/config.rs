//! 已合并配置的类型化读取。通常在 create 中读取并缓存；路径使用 JSON Pointer。
use crate::{ConfigScope, Kind, raw};
pub const OPTIONS: Data = Data(ConfigScope::Module);
/// Host-provided global presentation settings (currently preedit_type).
pub const SETTINGS: Data = Data(ConfigScope::Global);

#[derive(Clone, Copy)]
pub struct Data(ConfigScope);
impl Data {
    pub fn kind(self, path: &str) -> Kind {
        if path.len() > 1024 {
            return Kind::Missing;
        }
        Kind::try_from(unsafe { raw::data_kind(self.0 as i32, path.as_ptr(), path.len() as i32) })
            .unwrap_or(Kind::Missing)
    }
    pub fn len(self, path: &str) -> Option<usize> {
        if path.len() > 1024 {
            return None;
        }
        usize::try_from(unsafe { raw::data_len(self.0 as i32, path.as_ptr(), path.len() as i32) })
            .ok()
    }
    /// Integer values preserve all bits; cast to u64 for content_id.
    pub fn integer(self, path: &str) -> Option<i64> {
        if self.kind(path) != Kind::Number {
            return None;
        }
        Some(unsafe { raw::data_i64(self.0 as i32, path.as_ptr(), path.len() as i32) })
    }
    pub fn boolean(self, path: &str) -> Option<bool> {
        if self.kind(path) != Kind::Bool {
            return None;
        }
        Some(unsafe { raw::data_i64(self.0 as i32, path.as_ptr(), path.len() as i32) != 0 })
    }
    pub fn number(self, path: &str) -> Option<f64> {
        if self.kind(path) != Kind::Number {
            return None;
        }
        Some(unsafe { raw::data_number(self.0 as i32, path.as_ptr(), path.len() as i32) })
    }
    pub fn string(self, path: &str) -> Option<String> {
        if self.kind(path) != Kind::String {
            return None;
        }
        let len = self.len(path)?;
        if len > 1024 * 1024 {
            return None;
        }
        let mut bytes = vec![0; len];
        let copied = unsafe {
            raw::data_string(
                self.0 as i32,
                path.as_ptr(),
                path.len() as i32,
                bytes.as_mut_ptr(),
                len as i32,
            )
        };
        if copied != len as i32 {
            return None;
        }
        String::from_utf8(bytes).ok()
    }
}
