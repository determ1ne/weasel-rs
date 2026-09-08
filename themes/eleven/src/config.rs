use crate::bindings::Color;
use serde_json::Value;
use weasel_common::settings::ConfigSnapshot;

#[derive(Clone, Copy)]
enum Source {
    System,
    Fixed(Color),
}

#[derive(Clone)]
pub struct ThemeConfig {
    pub font: String,
    pub font_size: f64,
    accent: [Option<Source>; 2],
    background: [Option<Source>; 2],
    text: [Option<Source>; 2],
    notices: std::rc::Rc<std::cell::RefCell<Vec<crate::theme_api::ThemeNotice>>>,
}

fn fallback(
    notices: &mut Vec<crate::theme_api::ThemeNotice>,
    field: &str,
    value: &Value,
    reason: &str,
) {
    notices.push(crate::theme_api::ThemeNotice {
        severity: crate::theme_api::NoticeSeverity::Warning,
        code: "configuration.invalid".into(),
        message: format!("themeSettings.eleven.{field} 无效，已使用回退值。"),
        details: serde_json::json!({"field": field, "value": value.to_string().chars().take(512).collect::<String>(), "reason": reason}).to_string(),
    });
}

impl ThemeConfig {
    pub fn take_notices(&self) -> Vec<crate::theme_api::ThemeNotice> {
        self.notices.take()
    }
    pub fn load(settings: &ConfigSnapshot) -> Self {
        let mut notices = Vec::new();
        let object = settings.query(".themeSettings.eleven").ok().flatten();
        let get = |key: &str| object.and_then(|v| v.get(key)).unwrap_or(&Value::Null);
        let font = match get("font")
            .as_str()
            .filter(|s| !s.trim().is_empty() && s.len() <= 256 && !s.contains('\0'))
        {
            Some(font) => match font_exists(font) {
                Ok(true) => font.to_owned(),
                result => {
                    fallback(
                        &mut notices,
                        "font",
                        get("font"),
                        &format!("font unavailable ({result:?}); using Microsoft YaHei UI"),
                    );
                    "Microsoft YaHei UI".into()
                }
            },
            None => {
                fallback(
                    &mut notices,
                    "font",
                    get("font"),
                    "expected a nonempty font family; using Microsoft YaHei UI",
                );
                "Microsoft YaHei UI".into()
            }
        };
        let font_size = size(get("fontSize")).unwrap_or_else(|| {
            fallback(
                &mut notices,
                "fontSize",
                get("fontSize"),
                "expected small/medium/large/extraLarge or 1..256 DIP; using 14",
            );
            14.0
        });
        Self {
            font,
            font_size,
            accent: colors(&mut notices, "accentColor", get("accentColor")),
            background: colors(&mut notices, "backgroundColor", get("backgroundColor")),
            text: colors(&mut notices, "textColor", get("textColor")),
            notices: std::rc::Rc::new(std::cell::RefCell::new(notices)),
        }
    }

    pub fn accent(&self, dark: bool, original: Color) -> Color {
        match self.accent[dark as usize] {
            Some(Source::Fixed(color)) => color,
            Some(Source::System) => match system_accent() {
                Ok(color) => color,
                Err(error) => {
                    fallback(
                        &mut self.notices.borrow_mut(),
                        "accentColor",
                        &Value::String("system".into()),
                        &format!(
                            "DWM AccentColor unavailable: {error}; using previous palette default"
                        ),
                    );
                    original
                }
            },
            None => original,
        }
    }
    pub fn foreground(&self, dark: bool, original: Color) -> Color {
        match self.text[dark as usize] {
            Some(Source::Fixed(color)) => color,
            _ => original,
        }
    }
    pub fn background(&self, dark: bool) -> Option<Color> {
        match self.background[dark as usize] {
            Some(Source::Fixed(color)) => Some(color),
            _ => None,
        }
    }
}

fn size(value: &Value) -> Option<f64> {
    let n = match value.as_str() {
        Some("small") => 14.0,
        Some("medium") => 18.0,
        Some("large") => 22.0,
        Some("extraLarge") => 28.0,
        Some(_) => return None,
        None => value.as_f64()?,
    };
    (n.is_finite() && (1.0..=256.0).contains(&n)).then_some(n)
}

fn colors(
    notices: &mut Vec<crate::theme_api::ThemeNotice>,
    field: &str,
    value: &Value,
) -> [Option<Source>; 2] {
    fn parse(
        notices: &mut Vec<crate::theme_api::ThemeNotice>,
        field: &str,
        value: &Value,
    ) -> Option<Source> {
        let result = value.as_str().and_then(|s| {
            if s == "system" {
                Some(Source::System)
            } else {
                color(s).map(Source::Fixed)
            }
        });
        if result.is_none() {
            fallback(
                notices,
                field,
                value,
                "expected system or RGB/ARGB hexadecimal color; using original palette",
            );
        }
        result
    }
    if let Some(object) = value.as_object() {
        ["light", "dark"].map(|mode| {
            parse(
                notices,
                &format!("{field}.{mode}"),
                object.get(mode).unwrap_or(&Value::Null),
            )
        })
    } else {
        let color = parse(notices, field, value);
        [color, color]
    }
}

fn color(text: &str) -> Option<Color> {
    let hex = text.strip_prefix('#').unwrap_or(text);
    if !matches!(hex.len(), 6 | 8) || !hex.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let n = u32::from_str_radix(hex, 16).ok()?;
    Some(Color {
        A: if hex.len() == 6 { 255 } else { (n >> 24) as u8 },
        R: (n >> 16) as u8,
        G: (n >> 8) as u8,
        B: n as u8,
    })
}

fn system_accent() -> windows_core::Result<Color> {
    let key = windows_registry::CURRENT_USER.open(r"Software\Microsoft\Windows\DWM")?;
    let n = key.get_u32("AccentColor")?;
    // Registry ABGR differs from the user-facing AARRGGBB syntax.
    Ok(Color {
        A: (n >> 24) as u8,
        R: n as u8,
        G: (n >> 8) as u8,
        B: (n >> 16) as u8,
    })
}

fn font_exists(name: &str) -> windows_core::Result<bool> {
    use crate::d2d_bindings::*;
    unsafe {
        let factory: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
        let mut collection = None;
        factory
            .GetSystemFontCollection(&mut collection, false)
            .ok()?;
        let collection = collection.ok_or_else(|| {
            windows_core::Error::from_hresult(windows_core::HRESULT(0x80004005_u32 as i32))
        })?;
        let mut index = 0;
        let mut exists = Default::default();
        collection
            .FindFamilyName(
                &windows_strings::HSTRING::from(name),
                &mut index,
                &mut exists,
            )
            .ok()?;
        Ok(exists.as_bool())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_sizes_colors_and_independent_theme_fallbacks() {
        for (name, expected) in [
            ("small", 14.0),
            ("medium", 18.0),
            ("large", 22.0),
            ("extraLarge", 28.0),
        ] {
            assert_eq!(size(&Value::String(name.into())), Some(expected));
        }
        assert_eq!(size(&serde_json::json!(17.5)), Some(17.5));
        assert_eq!(size(&serde_json::json!(-1)), None);
        let c = color("#80123456").unwrap();
        assert_eq!([c.A, c.R, c.G, c.B], [0x80, 0x12, 0x34, 0x56]);
        assert_eq!(color("123456").unwrap().A, 255);
        assert!(color("#gg0000").is_none());
        let mut notices = Vec::new();
        let both = colors(
            &mut notices,
            "textColor",
            &serde_json::json!({"light":"broken","dark":"ffffff"}),
        );
        assert!(both[0].is_none());
        assert!(matches!(both[1], Some(Source::Fixed(_))));
        assert_eq!(notices.len(), 1);
        assert!(notices[0].message.contains("textColor.light"));
        // Even a failed creation can return its accumulated notices as data.
        let creation = crate::theme_api::ThemeCreation {
            backend: Err("native initialization failed".into()),
            notices,
        };
        assert!(creation.backend.is_err());
        assert_eq!(creation.notices.len(), 1);
    }
}
