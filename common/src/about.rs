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

/// 使用带信息图标的 Task Dialog 显示“关于”信息。
///
/// `owner` 为零时创建无父窗口的对话框。TIP 应继续调用 [`show`]，避免依赖宿主进程的
/// Common Controls 激活上下文；拥有独立清单的可执行程序可使用本函数。
pub fn show_task_dialog(owner: usize, text: &str) -> windows_core::Result<()> {
    crate::task_dialog::TaskDialog::new("关于小狼毫RS", "小狼毫RS")
        .owner(owner)
        .icon(crate::task_dialog::TaskDialogIcon::Information)
        .content(text)
        .show()
        .map(|_| ())
}
