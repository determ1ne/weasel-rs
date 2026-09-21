//! 普通日志不触发用户通知；需要用户处理的问题才使用 report_notice。
use crate::{raw, types};
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
