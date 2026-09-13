//! Installer command: request normal exit and observe the same process handle.
use crate::bindings::*;
use windows_strings::{PWSTR, w};

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
                WPARAM(weasel_common::broker_menu::EXIT as usize),
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
