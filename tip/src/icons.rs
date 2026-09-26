//! 提供任务栏主题判断和 TIP 配置文件所用图标资源索引。
//!
//! 配置文件图标标识保持固定；只有独立的中英文按钮图标随系统主题变化。
use crate::bindings::{COLOR_WINDOWTEXT, GetSysColor};

/// 判断 Windows 当前是否使用浅色系统主题；注册表不可用时按窗口文字颜色推断。
///
/// 查询失败会回退到系统颜色，不把个性化设置读取错误传播到 TSF 调用链。
pub(crate) fn taskbar_is_light() -> bool {
    windows_registry::CURRENT_USER
        .open(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize")
        .and_then(|key| key.get_u32("SystemUsesLightTheme"))
        .map(|value| value != 0)
        .unwrap_or_else(|_| {
            let text = unsafe { GetSysColor(COLOR_WINDOWTEXT) };
            let brightness = (text & 0xff) + ((text >> 8) & 0xff) + ((text >> 16) & 0xff);
            brightness < 3 * 128
        })
}

/// `icons.rc` 中 `GROUP_ICON` 的从零开始索引，而非单张图像的资源 ID。
/// 配置文件身份图标固定使用此索引，不随主题切换。
pub(crate) const PROFILE_ICON_INDEX: u32 = 0;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_index_selects_fixed_branding_icon_in_resource_order() {
        let mut names: Vec<_> = include_str!("../icons.rc")
            .lines()
            .filter_map(|line| line.split_once(" ICON ").map(|(name, _)| name.trim()))
            .collect();
        names.sort_unstable();
        assert_eq!(names[PROFILE_ICON_INDEX as usize], "BRAND");
        assert!(include_str!("../icons.rc").contains("BRAND ICON WEASEL_ICON_PATH"));
    }
}
