//! 复用语言栏菜单的 broker 命令入口；只投递消息，不等待服务重启阻塞 UI。
use crate::bindings::*;
use weasel_common::command_menu;

/// 向已运行的 broker 投递重启服务命令，不等待重启完成。
///
/// 找不到 broker 窗口或 Windows 拒绝投递时返回错误；成功仅表示消息已入队，
/// 不代表配置已应用或服务已重启。
pub fn request_restart() -> Result<(), String> {
    let class = windows_strings::HSTRING::from(command_menu::BROKER_WINDOW_CLASS);
    unsafe {
        let broker = FindWindowW(windows_core::PCWSTR(class.as_ptr()), None);
        if broker.0.is_null() {
            return Err("算法服务管理程序未运行，请先启动算法服务。".into());
        }
        if !PostMessageW(
            Some(broker),
            WM_COMMAND as u32,
            WPARAM(command_menu::RESTART as usize),
            LPARAM(0),
        )
        .as_bool()
        {
            return Err(windows_core::Error::from_thread().to_string());
        }
    }
    Ok(())
}
