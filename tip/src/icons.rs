//! Icon colors refer to the glyph, so a dark taskbar needs a light icon.
use crate::bindings::{COLOR_WINDOWTEXT, GetSysColor};

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

// Zero-based GROUP_ICON index in icons.rc, not an individual image resource ID.
// The profile identity stays fixed; only the separate EN/ZH button follows theme.
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
