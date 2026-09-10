//! 框架无关的表单字段。有限控件覆盖常见字段，复杂 union/字典保留 JSON 编辑。
use crate::{catalog::Section, config::Document};
use serde_json::Value;
use weasel_common::settings::merge;

pub struct Field {
    pub path: Vec<String>,
    pub title: String,
    pub key: String,
    pub description: String,
    pub values: Vec<Value>,
    pub choices: Vec<String>,
    pub selected: i32,
    pub text: String,
    pub kind: String,
    pub boolean: bool,
    pub multiline: bool,
    pub inherited_text: String,
    pub inherited_selected: i32,
}
pub fn at<'a>(value: &'a Value, path: &[String]) -> Option<&'a Value> {
    path.iter()
        .try_fold(value, |node, key| node.as_object()?.get(key))
}
pub fn assign(root: &mut Value, path: &[String], value: Option<Value>) -> Result<(), String> {
    if path.is_empty() {
        *root = value.unwrap_or_else(|| serde_json::json!({}));
        return Ok(());
    }
    let mut node = root;
    for key in &path[..path.len() - 1] {
        let object = node
            .as_object_mut()
            .ok_or("父级配置不是对象，请先修复 JSON")?;
        if !object.contains_key(key) && value.is_none() {
            return Ok(());
        }
        node = object
            .entry(key.clone())
            .or_insert_with(|| serde_json::json!({}));
    }
    let object = node
        .as_object_mut()
        .ok_or("父级配置不是对象，请先修复 JSON")?;
    let key = &path[path.len() - 1];
    if let Some(value) = value {
        object.insert(key.clone(), value);
    } else {
        object.remove(key);
    }
    Ok(())
}
pub fn effective(section: &Section, doc: &Document) -> Value {
    let mut value = section.metadata.defaults.clone();
    if let Some(base) = at(&doc.base, &section.mount) {
        merge(&mut value, base.clone());
    }
    if let Some(patch) = at(&doc.patch, &section.mount) {
        merge(&mut value, patch.clone());
    }
    value
}
fn display(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string())
}
pub fn fields(section: &Section, doc: &Document) -> Result<Vec<Field>, String> {
    let mut base = section.metadata.defaults.clone();
    if let Some(value) = at(&doc.base, &section.mount) {
        merge(&mut base, value.clone());
    }
    let mut out = Vec::new();
    collect(
        &section.metadata.richschema["schema"],
        &section.metadata.richschema["ui"],
        &section.mount,
        &[],
        &base,
        doc,
        &mut out,
        0,
    )?;
    Ok(out)
}
fn collect(
    schema: &Value,
    ui: &Value,
    mount: &[String],
    local: &[String],
    base: &Value,
    doc: &Document,
    out: &mut Vec<Field>,
    depth: usize,
) -> Result<(), String> {
    if depth > 16 || out.len() >= 256 {
        return Err("表单深度或字段数量超限".into());
    }
    if let Some(properties) = schema["properties"].as_object() {
        let mut entries: Vec<_> = properties.iter().collect();
        entries.sort_by_key(|(key, _)| {
            let mut path = local.to_vec();
            path.push((*key).clone());
            ui["fields"][pointer(&path)]["order"].as_i64().unwrap_or(0)
        });
        for (key, child) in entries {
            if key == "$schema" || (mount.is_empty() && local.is_empty() && key == "themeSettings")
            {
                continue;
            }
            let mut next = local.to_vec();
            next.push(key.clone());
            collect(child, ui, mount, &next, base, doc, out, depth + 1)?;
        }
        return Ok(());
    }
    let mut path = mount.to_vec();
    path.extend_from_slice(local);
    let inherited = at(base, local).cloned().unwrap_or(Value::Null);
    let current = at(&doc.patch, &path);
    let font_size = mount == ["themeSettings", "eleven"] && local == ["fontSize"];
    let color_input = mount == ["themeSettings", "eleven"]
        && local.len() == 1
        && ["accentColor", "backgroundColor", "textColor"].contains(&local[0].as_str());
    let rgb_color = mount == ["themeSettings", "wasm", "modules", "weaselui", "config"]
        && local.len() == 2
        && local[0] == "color";
    let values = if font_size {
        vec!["small", "medium", "large", "extraLarge"]
            .into_iter()
            .map(|v| Value::String(v.into()))
            .collect()
    } else if schema["type"] == "boolean" {
        vec![Value::Bool(false), Value::Bool(true)]
    } else {
        schema["enum"].as_array().cloned().unwrap_or_default()
    };
    let kind = if rgb_color {
        "rgb_color"
    } else if color_input {
        "color"
    } else if font_size {
        "font_size"
    } else if !values.is_empty() {
        "choice"
    } else if schema["type"] == "string" {
        "string"
    } else {
        "json"
    };
    let mut choices = vec![format!(
        "继承（{}）",
        display(&inherited).chars().take(100).collect::<String>()
    )];
    let mut values = values;
    if kind == "choice" || font_size {
        choices.extend(values.iter().map(display));
        if font_size {
            choices.push("自定义".into());
        }
    } else {
        choices.push("覆盖".into());
    }
    let selected = match current {
        None => 0,
        Some(value) if font_size => values
            .iter()
            .position(|v| v == value)
            .map(|i| i as i32 + 1)
            .unwrap_or(5),
        Some(value) if kind == "choice" => match values.iter().position(|v| v == value) {
            Some(i) => (i + 1) as i32,
            None => {
                values.push(value.clone());
                choices.push("原值（不符合当前约束）".into());
                values.len() as i32
            }
        },
        Some(_) => 1,
    };
    let value = current.unwrap_or(&inherited);
    out.push(Field {
        key: if local.is_empty() {
            "config".into()
        } else {
            local.join(".")
        },
        path,
        title: ui["fields"][pointer(local)]["title"]
            .as_str()
            .or_else(|| schema["title"].as_str())
            .map(str::to_owned)
            .unwrap_or_else(|| {
                if local.is_empty() {
                    "配置（JSON）".into()
                } else {
                    local.join(" / ")
                }
            }),
        description: format!(
            "{}{}",
            schema["description"].as_str().unwrap_or(""),
            if kind == "json" { "（JSON 值）" } else { "" }
        ),
        choices,
        values,
        selected,
        text: if kind == "json" && schema["type"] == "object" {
            serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
        } else if font_size {
            if value.is_number() {
                value.to_string()
            } else {
                "14".into()
            }
        } else if kind == "string" {
            display(value)
        } else {
            value.to_string()
        },
        kind: kind.into(),
        boolean: schema["type"] == "boolean",
        multiline: kind == "json" && schema["type"] == "object",
        inherited_text: if kind == "json" && schema["type"] == "object" {
            serde_json::to_string_pretty(&inherited).unwrap_or_else(|_| inherited.to_string())
        } else if font_size {
            if inherited.is_number() {
                inherited.to_string()
            } else {
                "14".into()
            }
        } else if kind == "string" {
            display(&inherited)
        } else {
            inherited.to_string()
        },
        inherited_selected: if font_size {
            ["small", "medium", "large", "extraLarge"]
                .iter()
                .position(|s| inherited.as_str() == Some(s))
                .map(|i| i as i32)
                .unwrap_or(4)
        } else {
            values_for_index(schema, &inherited)
        },
    });
    Ok(())
}
fn pointer(path: &[String]) -> String {
    path.iter()
        .map(|s| format!("/{}", s.replace('~', "~0").replace('/', "~1")))
        .collect()
}
impl Field {
    pub fn display_selected(&self) -> i32 {
        if self.selected == 0 {
            self.inherited_selected
        } else {
            self.selected - 1
        }
    }
    pub fn value(&self, selected: i32, text: &str) -> Result<Option<Value>, String> {
        if selected == 0 {
            return Ok(None);
        }
        if self.kind == "font_size" && selected == 5 {
            let size: f64 = text.trim().parse().map_err(|_| "请输入有效字号（1–256）")?;
            if !size.is_finite() || !(1.0..=256.0).contains(&size) {
                return Err("字号必须在 1–256 之间".into());
            }
            return Ok(Some(serde_json::json!(size)));
        }
        if self.kind == "choice" || self.kind == "font_size" {
            return self
                .values
                .get((selected - 1) as usize)
                .cloned()
                .map(Some)
                .ok_or("无效选项".into());
        }
        if text.len() > 16384 {
            return Err("单个字段超过 16 KiB，请使用配置文件编辑".into());
        }
        if self.kind == "string" {
            Ok(Some(Value::String(text.into())))
        } else {
            serde_json::from_str(text)
                .map(Some)
                .map_err(|e| e.to_string())
        }
    }
}

fn values_for_index(schema: &Value, value: &Value) -> i32 {
    if schema["type"] == "boolean" {
        return i32::from(value == true);
    }
    schema["enum"]
        .as_array()
        .and_then(|a| a.iter().position(|v| v == value))
        .map(|i| i as i32)
        .unwrap_or(-1)
}
