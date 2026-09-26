//! 注册设置界面的颜色编解码，将 JSON 字符串、系统色和浅深色对象映射到 UI 值。
//! JSON 结构转换在宿主侧完成，模块同时提供十六进制颜色解析与格式化。
use crate::{Channels, ColorCodec, ColorValue, ParsedColor, SettingsWindow};
use serde_json::{Value, json};
use slint::{Color, ComponentHandle};

/// 从 JSON 字符串颜色解析 UI 颜色；输入不合法时采用调用方给定的回退色。
fn parse(value: &Value, fallback: Color) -> Color {
    value.as_str().and_then(parse_hex).unwrap_or(fallback)
}
/// 解析 6 位 RGB 或 8 位 ARGB 十六进制颜色，可带 `#` 和外围空白。
///
/// RGB 输入补全不透明 alpha；格式错误或非十六进制字符返回 `None`。
fn parse_hex(text: &str) -> Option<Color> {
    let text = text.trim();
    let text = text.strip_prefix('#').unwrap_or(text);
    if !matches!(text.len(), 6 | 8) || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let number = u32::from_str_radix(text, 16).ok()?;
    match text.len() {
        6 => Some(Color::from_argb_encoded(0xff000000 | number)),
        8 => Some(Color::from_argb_encoded(number)),
        _ => None,
    }
}
/// 将 Slint 颜色编码为大写的 `#AARRGGBB` 文本。
fn hex(color: Color) -> String {
    format!("#{:08X}", color.as_argb_encoded())
}
/// 注册 UI 与 JSON 颜色值之间的编解码及辅助回调。
///
/// 解码识别系统色、单色和浅/深色对象；无法解析的颜色分别使用预设回退色。编码按
/// mode 输出相应 JSON 形状，通道回调将 Slint 通道转换为浮点分量。
pub fn install(ui: &SettingsWindow) {
    let codec = ui.global::<ColorCodec>();
    codec.on_decode(|text| {
        let value: Value = serde_json::from_str(text.as_str()).unwrap_or(Value::Null);
        let light = if value.is_object() {
            &value["light"]
        } else {
            &value
        };
        let dark = if value.is_object() {
            &value["dark"]
        } else {
            &value
        };
        ColorValue {
            mode: if value == "system" {
                0
            } else if value.is_object() {
                2
            } else {
                1
            },
            light: parse(light, Color::from_argb_encoded(0xff0078d4)),
            dark: parse(dark, Color::from_argb_encoded(0xff60cdff)),
            light_system: light == "system",
            dark_system: dark == "system",
        }
    });
    codec.on_encode(|value| {
        let light = if value.light_system {
            "system".into()
        } else {
            hex(value.light)
        };
        let dark = if value.dark_system {
            "system".into()
        } else {
            hex(value.dark)
        };
        match value.mode {
            0 => json!("system"),
            2 => json!({"light":light, "dark":dark}),
            _ => json!(hex(value.light)),
        }
        .to_string()
        .into()
    });
    codec.on_channels(|c| Channels {
        red: c.red() as f32,
        green: c.green() as f32,
        blue: c.blue() as f32,
        alpha: c.alpha() as f32,
    });
    codec.on_hex(|c| hex(c).into());
    codec.on_parse_hex(|text| {
        let value = parse_hex(text.as_str());
        ParsedColor {
            valid: value.is_some(),
            value: value.unwrap_or_default(),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hex_input_preserves_argb_and_rejects_invalid_text() {
        assert_eq!(hex(parse_hex("#12abEF").unwrap()), "#FF12ABEF");
        assert_eq!(hex(parse_hex(" 80123456 ").unwrap()), "#80123456");
        for invalid in ["", "#123", "+12345", "#1234567", "#gg1122"] {
            assert!(parse_hex(invalid).is_none());
        }
    }
}
