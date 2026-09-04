//! Shell actions run outside the tray message loop and never inside the TIP.
use crate::bindings::*;
use weasel_common::{broker_menu::*, runtime_paths};
use windows_strings::{HSTRING, PCWSTR, w};

fn target(command: u32) -> Result<HSTRING, String> {
    match command {
        HELP => Ok(HSTRING::from("https://rime.im/docs/")),
        FORUM => Ok(HSTRING::from("https://rime.im/discuss/")),
        USER_DIRECTORY | PROGRAM_DIRECTORY | LOG_DIRECTORY => {
            let paths = runtime_paths::RuntimePaths::discover().map_err(|e| e.to_string())?;
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

pub fn open(command: u32) {
    let result = std::thread::Builder::new()
        .name("weasel-shell".into())
        .spawn(move || {
            let result = (|| -> Result<(), String> {
                let target = target(command)?;
                unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED as u32).ok() }
                    .map_err(|e| e.to_string())?;
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
                unsafe {
                    CoUninitialize();
                }
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn links_match_requested_destinations() {
        assert_eq!(
            target(HELP).unwrap(),
            HSTRING::from("https://rime.im/docs/")
        );
        assert_eq!(
            target(FORUM).unwrap(),
            HSTRING::from("https://rime.im/discuss/")
        );
        assert!(target(0).is_err());
    }
}
