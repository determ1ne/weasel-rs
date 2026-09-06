//! Always-on connection failures, independent of development mode and RPC.
//! Fixed-size UTF-16 output: never format key events, preedit or commit text.
use std::fmt::{self, Write};

struct DebugLine {
    units: [u16; 1024],
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
