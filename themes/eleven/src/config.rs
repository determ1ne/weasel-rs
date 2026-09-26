//! eleven 主题设置的解析、系统值读取与无效配置告警。
//!
//! 配置从 `themeSettings.eleven` 快照读取；不合法或不可用的字段独立回退，
//! 并将告警保存在可克隆共享的缓冲区中，供创建流程收集。

use crate::bindings::Color;
use serde_json::Value;
use weasel_common::settings::ConfigSnapshot;

#[derive(Clone, Copy)]
enum Source {
    /// 使用对应模式的系统调色板值；强调色从系统读取，其他颜色沿用主题默认值。
    System,
    /// 使用配置中解析出的固定 RGBA 颜色。
    Fixed(Color),
}

/// 解析后的 eleven 主题配置及其待交付告警。
///
/// `accent`、`background` 和 `text` 的两个槽位依次对应浅色与深色模式；
/// 配置值缺失时保留由系统调色板提供的默认值。告警缓冲区通过 `Rc<RefCell<_>>`
/// 与克隆共享，适用于同一 UI 线程上的创建与主题初始化流程，不支持跨线程访问。
#[derive(Clone)]
pub struct ThemeConfig {
    /// 已验证存在的字体族名称，必要时回退到 Microsoft YaHei UI。
    pub font: String,
    /// 候选文字字号，单位为 DIP。
    pub font_size: f64,
    /// 浅色、深色模式各自的强调色来源。
    accent: [Option<Source>; 2],
    /// 浅色、深色模式各自的背景色来源。
    background: [Option<Source>; 2],
    /// 浅色、深色模式各自的文字色来源。
    text: [Option<Source>; 2],
    /// 尚未交付的配置/系统值告警；克隆共享，取出时整体清空。
    notices: std::rc::Rc<std::cell::RefCell<Vec<crate::theme_api::ThemeNotice>>>,
}

/// 将配置问题追加为结构化警告，并截断回显值以限制诊断数据大小。
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
    /// 取出此前累积的告警并清空共享缓冲区。
    pub fn take_notices(&self) -> Vec<crate::theme_api::ThemeNotice> {
        self.notices.take()
    }

    /// 从设置快照读取并校验 eleven 配置；每个无效字段单独回退并产生告警。
    ///
    /// 字体需在系统字体集合中存在；字号限于 1 至 256 DIP；颜色可按单值或
    /// `light`/`dark` 分别指定。缺失、格式错误或字体不可用时使用相应默认值，
    /// 其余字段仍继续解析。
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

    /// 按明暗模式解析强调色；系统色读取失败时告警并返回调用方提供的原色。
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
    /// 按明暗模式选取文字色；系统来源或缺省配置沿用原调色板文字色。
    pub fn foreground(&self, dark: bool, original: Color) -> Color {
        match self.text[dark as usize] {
            Some(Source::Fixed(color)) => color,
            _ => original,
        }
    }
    /// 返回该模式显式配置的背景色；系统来源和缺省值均交由主题背景逻辑处理。
    pub fn background(&self, dark: bool) -> Option<Color> {
        match self.background[dark as usize] {
            Some(Source::Fixed(color)) => Some(color),
            _ => None,
        }
    }
}

/// 将字号枚举或数值转换为有限且位于允许区间内的 DIP 值。
///
/// 非数字、未知名称、非有限值及越界值均返回 `None`，由配置加载器统一告警回退。
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

/// 解析单个颜色设置或分别给出的浅色/深色颜色，并逐项记录无效值。
///
/// 对象中一侧无效不会影响另一侧；非对象值被两个模式共同使用。无效项返回
/// `None`，表示使用对应模式的原调色板颜色。
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

/// 解析可选 `#` 前缀的 RGB 或 ARGB 十六进制文本；RGB 输入的不透明度为满值。
///
/// 仅接受 6 或 8 个 ASCII 十六进制字符，格式错误时返回 `None`。
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

/// 从当前用户 DWM 注册表读取系统强调色，并将注册表 ABGR 转为 RGBA 通道。
///
/// 注册表打开或值读取失败均保留为 `windows_core::Error`，由调用方回退并告警。
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

/// 使用共享 DirectWrite 工厂查询系统字体集合中是否存在指定字体族。
///
/// 返回 `false` 表示字体族不存在；COM/DirectWrite 查询失败作为错误交由加载器
/// 采用默认字体并记录告警。共享工厂避免每次配置检查创建独立字体基础设施。
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
