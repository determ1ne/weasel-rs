//! 记录与开发模式无关的 RPC 连接故障，直接输出到 Windows 调试器。
//!
//! 行缓冲区固定为 UTF-16 容量，禁止记录按键、预编辑和提交文本；诊断格式化和写出均有界，
//! 避免连接失败路径意外暴露用户输入或分配无界缓冲区。
use std::fmt::{self, Write};

struct DebugLine {
    /// 留出终止 NUL 的固定容量缓冲区。
    units: [u16; 1024],
    /// 当前有效 UTF-16 单元数，不含末尾 NUL。
    len: usize,
}
impl Default for DebugLine {
    fn default() -> Self {
        Self {
            units: [0; 1024],
            len: 0,
        }
    }
}
impl Write for DebugLine {
    /// 编码完整 Unicode 标量；换行和 NUL 改为空格，容量不足时整体报告格式化失败。
    fn write_str(&mut self, text: &str) -> fmt::Result {
        for ch in text.chars() {
            let ch = if matches!(ch, '\0' | '\r' | '\n') {
                ' '
            } else {
                ch
            };
            let mut encoded = [0; 2];
            let units = ch.encode_utf16(&mut encoded);
            if self.len + units.len() > self.units.len() - 2 {
                return Err(fmt::Error);
            }
            self.units[self.len..self.len + units.len()].copy_from_slice(units);
            self.len += units.len();
        }
        Ok(())
    }
}

/// 输出 RPC 连接阶段、错误及可选管道名，不包含输入事件或编辑文本。
///
/// 即使格式化超出固定容量，也只输出已安全编码的前缀；缓冲区始终保留 NUL 终止空间。
pub fn report(stage: &str, pipe: Option<&str>, error: impl fmt::Display) {
    let mut line = DebugLine::default();
    let _ = write!(
        line,
        "weasel-tip: rpc failure pid={} thread={:?} stage={} reason={} pipe={}",
        std::process::id(),
        std::thread::current().id(),
        stage,
        error,
        pipe.unwrap_or("<unresolved>")
    );
    line.units[line.len] = b'\n' as u16;
    unsafe {
        crate::bindings::OutputDebugStringW(windows_strings::PCWSTR(line.units.as_ptr()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn output_is_bounded_terminated_and_utf16_safe() {
        let mut line = DebugLine::default();
        assert!(write!(line, "{}", "🙂".repeat(1024)).is_err());
        assert!(line.len <= 1022);
        assert!(String::from_utf16(&line.units[..line.len]).is_ok());
        assert_eq!(line.units[line.len], 0);
        let mut line = DebugLine::default();
        write!(line, "a\0b\nc\rd").unwrap();
        assert_eq!(
            String::from_utf16(&line.units[..line.len]).unwrap(),
            "a b c d"
        );
    }
}
