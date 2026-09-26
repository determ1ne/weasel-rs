//! 向宿主输出诊断信息。
//!
//! `log` 适用于调试和运行状态；只有确实需要用户采取行动时才调用 `report_notice`，
//! 后者会进入用户可见的通知通道。消息通过同步 ABI 复制，调用期间须保持输入字符串有效。
use crate::{raw, types};
/// 以信息级别记录一条日志，不触发用户通知。
pub fn log(message: &str) {
    unsafe {
        raw::log(
            types::LogLevel::Info as i32,
            message.as_ptr(),
            message.len() as i32,
        );
    }
}

/// 需要用户处理的问题；普通调试信息请使用 log，不要触发用户通知。
pub fn report_notice(message: &str) {
    unsafe {
        raw::report_notice(message.as_ptr(), message.len() as i32);
    }
}
