//! “关于”对话框相关功能的实现

use windows_strings::{HSTRING, PCWSTR, w};

/// 判断调用线程最近取得的键盘状态中 Shift 是否处于按下状态。
pub fn shift_pressed() -> bool {
    unsafe { crate::bindings::GetKeyState(crate::bindings::VK_SHIFT as i32) < 0 }
}

/// 生成指定组件的本地构建与运行环境摘要。
pub fn information(component: &str) -> String {
    let os = windows_version::OsVersion::current();
    format!(
        "小狼毫RS {}\n组件：{component}\n构建时间（UTC）：{}\n源码指纹（Git commit）：{}\n源码状态：{}\n目标：{}\nWindows：{}.{}.{}\nPID：{}\n构建标识：{}/{}/{}\n",
        env!("CARGO_PKG_VERSION"),
        env!("WEASEL_BUILD_DATE"),
        env!("WEASEL_BUILD_REVISION"),
        env!("WEASEL_BUILD_STATE"),
        env!("WEASEL_BUILD_TARGET"),
        os.major,
        os.minor,
        os.build,
        std::process::id(),
        env!("WEASEL_BUILD_REVISION"),
        env!("WEASEL_BUILD_EPOCH"),
        env!("WEASEL_BUILD_TARGET"),
    )
}

/// 使用Windows 消息框显示“关于”信息。
pub fn show(text: &str) {
    let text = HSTRING::from(text.replace('\n', "\r\n"));
    unsafe {
        let _ = crate::bindings::MessageBoxW(
            None,
            PCWSTR(text.as_ptr()),
            w!("关于小狼毫RS"),
            (crate::bindings::MB_OK | crate::bindings::MB_SETFOREGROUND) as u32,
        );
    }
}
