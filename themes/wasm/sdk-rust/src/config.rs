//! 读取宿主已合并的主题配置和允许主题访问的全局展示设置。
//!
//! 在 `theme_create` 读取并缓存稳定配置通常最合适；路径采用 RFC 风格 JSON Pointer，
//! 不是键路径表达式。仅能访问 `OPTIONS` 的模块选项和 `SETTINGS` 暴露的全局字段，
//! 不会因此获得其他应用配置。读取结果为 `None`/`Missing` 时应使用主题自己的字段级回退。
use crate::{ConfigScope, Kind, raw};
/// 此主题模块的合并后选项。
pub const OPTIONS: Data = Data(ConfigScope::Module);
/// 宿主允许主题读取的全局展示设置（当前包括 `preedit_type`）。
pub const SETTINGS: Data = Data(ConfigScope::Global);

/// 在一个受限配置范围内执行类型化 JSON Pointer 查询。
///
/// 该值不持有配置副本；每次方法调用都会同步查询当前主题实例的只读配置。
#[derive(Clone, Copy)]
pub struct Data(ConfigScope);
impl Data {
    /// 返回路径对应的数据种类；无效路径、超长路径或未知宿主结果按 `Missing` 处理。
    pub fn kind(self, path: &str) -> Kind {
        if path.len() > 1024 {
            return Kind::Missing;
        }
        Kind::try_from(unsafe { raw::data_kind(self.0 as i32, path.as_ptr(), path.len() as i32) })
            .unwrap_or(Kind::Missing)
    }
    /// 返回字符串的 UTF-8 字节数或数组/对象的元素数；路径不存在或类型不支持时返回 `None`。
    pub fn len(self, path: &str) -> Option<usize> {
        if path.len() > 1024 {
            return None;
        }
        usize::try_from(unsafe { raw::data_len(self.0 as i32, path.as_ptr(), path.len() as i32) })
            .ok()
    }
    /// 读取整数；必须先匹配 `Kind::Number`，因此缺失、类型不符与合法的零值可区分。
    /// 整数按 `i64` 位模式返回；读取无符号 64 位标识时可将结果转换为 `u64`。
    pub fn integer(self, path: &str) -> Option<i64> {
        if self.kind(path) != Kind::Number {
            return None;
        }
        Some(unsafe { raw::data_i64(self.0 as i32, path.as_ptr(), path.len() as i32) })
    }
    /// 读取布尔值；路径类型不是 `Bool` 时返回 `None`。
    pub fn boolean(self, path: &str) -> Option<bool> {
        if self.kind(path) != Kind::Bool {
            return None;
        }
        Some(unsafe { raw::data_i64(self.0 as i32, path.as_ptr(), path.len() as i32) != 0 })
    }
    /// 读取数值为 `f64`；路径类型不是 `Number` 时返回 `None`。
    pub fn number(self, path: &str) -> Option<f64> {
        if self.kind(path) != Kind::Number {
            return None;
        }
        Some(unsafe { raw::data_number(self.0 as i32, path.as_ptr(), path.len() as i32) })
    }
    /// 复制字符串为拥有所有权的 Rust `String`。
    ///
    /// 字符串最多复制 1 MiB；超限、UTF-8 无效或读取期间长度不一致时返回 `None`。
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
