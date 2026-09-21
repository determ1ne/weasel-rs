use weasel_wasm_sdk::{Kind, config::OPTIONS, diagnostics::report_notice};

pub struct Style {
    pub font_size: f32,
    pub animations: bool,
    light: u32,
    dark: u32,
}
impl Style {
    pub fn read() -> Self {
        let size = OPTIONS.number("/fontSize");
        if OPTIONS.kind("/fontSize") != Kind::Missing
            && !size.is_some_and(|v| v.is_finite() && (12.0..=24.0).contains(&v))
        {
            report_notice("Orbit: fontSize 必须为 12–24，已使用 16。");
        }
        let animations = OPTIONS.boolean("/animations");
        if animations.is_none() && OPTIONS.kind("/animations") != Kind::Missing {
            report_notice("Orbit: animations 必须为布尔值，已启用动画。");
        }
        Self {
            font_size: size
                .filter(|v| v.is_finite() && (12.0..=24.0).contains(v))
                .unwrap_or(16.0) as f32,
            animations: animations.unwrap_or(true),
            light: color("/accentLight", 0xff527da6),
            dark: color("/accentDark", 0xff9bc3e8),
        }
    }
    pub fn palette(&self, dark: bool) -> Palette {
        if dark {
            Palette {
                background: 0xff202a39,
                border: 0xff3e4b60,
                text: 0xffeef1f5,
                muted: 0xffa5b1c4,
                accent: self.dark,
            }
        } else {
            Palette {
                background: 0xfffaf8f3,
                border: 0xffd8dfe7,
                text: 0xff26384b,
                muted: 0xff748295,
                accent: self.light,
            }
        }
    }
}
fn color(path: &str, fallback: u32) -> u32 {
    if OPTIONS.kind(path) == Kind::Missing {
        return fallback;
    }
    if let Some(value) = OPTIONS.string(path) {
        let value = value.strip_prefix('#').unwrap_or(&value);
        if value.len() == 6 && value.bytes().all(|b| b.is_ascii_hexdigit()) {
            if let Ok(rgb) = u32::from_str_radix(value, 16) {
                return 0xff000000 | rgb;
            }
        }
    }
    report_notice(&format!(
        "Orbit: {path} 必须为 RGB 十六进制颜色，已使用默认颜色。"
    ));
    fallback
}
pub struct Palette {
    pub background: u32,
    pub border: u32,
    pub text: u32,
    pub muted: u32,
    pub accent: u32,
}
