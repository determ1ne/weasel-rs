//! 执行打开设置、帮助和目录等 Shell 菜单操作。
//!
//! 这些动作在独立线程中处理，避免阻塞托盘消息循环；调用边界也确保它们不在文本输入处理器（TIP）中运行。
use crate::bindings::*;
use weasel_common::comrt::ComApartment;
use weasel_common::{command_menu::*, process::RuntimePaths};
use windows_strings::{HSTRING, PCWSTR, w};

/// 将菜单命令解析为可交给 Windows Shell 打开的目标。
///
/// 设置程序缺失时回退到用户目录；用户数据和日志目录会按需创建，程序目录则必须已经存在。
/// 路径发现、目录创建或不支持的命令以错误返回，调用方负责向用户展示。
fn target(command: u32) -> Result<HSTRING, String> {
    match command {
        SETTINGS => {
            let paths = RuntimePaths::discover().map_err(|e| e.to_string())?;
            let executable = paths.executable_directory.join("weasel-settings.exe");
            if !executable.is_file() {
                return target(USER_DIRECTORY);
            }
            Ok(HSTRING::from(executable.as_os_str()))
        }
        HELP => Ok(HSTRING::from("https://rime.im/docs/")),
        FORUM => Ok(HSTRING::from("https://rime.im/discuss/")),
        USER_DIRECTORY | PROGRAM_DIRECTORY | LOG_DIRECTORY => {
            let paths = RuntimePaths::discover().map_err(|e| e.to_string())?;
            let path = match command {
                USER_DIRECTORY => paths.user_data,
                LOG_DIRECTORY => paths.logs,
                _ => paths.executable_directory,
            };
            if command != PROGRAM_DIRECTORY {
                std::fs::create_dir_all(&path).map_err(|e| e.to_string())?;
            }
            Ok(HSTRING::from(path.as_os_str()))
        }
        _ => Err("不支持的菜单命令".to_owned()),
    }
}

/// 在专用线程中打开菜单目标，并将启动线程或 Shell 失败显示为错误对话框。
///
/// 工作线程初始化单线程 COM apartment，完成 Shell 调用后再反初始化；本函数不等待打开操作结束。
pub fn open(command: u32) {
    let result = std::thread::Builder::new()
        .name("weasel-shell".into())
        .spawn(move || {
            let result = (|| -> Result<(), String> {
                let target = target(command)?;
                let _apartment = ComApartment::initialize_sta().map_err(|e| e.to_string())?;
                let result = unsafe {
                    ShellExecuteW(
                        None,
                        w!("open"),
                        PCWSTR(target.as_ptr()),
                        None,
                        None,
                        SW_SHOWNORMAL,
                    )
                };
                if result.0 as isize <= 32 {
                    return Err(format!("无法打开目标，Shell 错误码：{}", result.0 as isize));
                }
                Ok(())
            })();
            if let Err(error) = result {
                show_error(&error);
            }
        });
    if let Err(error) = result {
        show_error(&format!("无法启动菜单操作：{error}"));
    }
}

/// 使用前台错误对话框呈现菜单操作失败原因。
fn show_error(error: &str) {
    let error = HSTRING::from(error);
    unsafe {
        let _ = MessageBoxW(
            None,
            PCWSTR(error.as_ptr()),
            w!("Weasel-RS"),
            (MB_OK | MB_ICONERROR | MB_SETFOREGROUND) as u32,
        );
    }
}
