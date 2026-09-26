//! 通过窗口命令请求 broker 执行正常关闭，并等待最初识别的进程退出；在发送命令前
//! 校验进程映像路径，防止安装程序关闭来自另一安装目录的 broker。
use crate::bindings::*;
use windows_strings::{PWSTR, w};

/// 请求指定安装目录中的 broker 正常退出，并等待其进程结束。
///
/// 没有 broker 窗口时视为无需关闭。找到窗口后会固定其进程句柄、校验可执行文件
/// 路径，再发送退出命令；无法打开进程、路径不符或 30 秒内未退出均返回错误。
pub fn run(directory: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    unsafe {
        let hwnd = FindWindowW(w!("weasel-rs-broker"), None);
        if hwnd.0.is_null() {
            return Ok(());
        }
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let handle = OpenProcess(
            (PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE) as u32,
            false,
            pid,
        );
        if handle.0.is_null() {
            return Err(std::io::Error::last_os_error().into());
        }
        let result = (|| -> Result<(), Box<dyn std::error::Error>> {
            let mut path = vec![0u16; 32768];
            let mut len = path.len() as u32;
            QueryFullProcessImageNameW(handle, 0, PWSTR(path.as_mut_ptr()), &mut len).ok()?;
            let actual = std::path::PathBuf::from(String::from_utf16(&path[..len as usize])?);
            if actual.canonicalize()? != directory.join("weasel-broker.exe").canonicalize()? {
                return Err("running broker belongs to another installation".into());
            }
            PostMessageW(
                Some(hwnd),
                WM_COMMAND as u32,
                WPARAM(weasel_common::command_menu::EXIT as usize),
                LPARAM(0),
            )
            .ok()?;
            if WaitForSingleObject(handle, 30000) != WAIT_OBJECT_0 as u32 {
                return Err("broker did not exit within 30 seconds".into());
            }
            Ok(())
        })();
        let _ = CloseHandle(handle);
        result
    }
}
