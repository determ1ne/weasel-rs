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
    pub allow_system_color: bool,
    pub adaptive_color: bool,
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
        &section.metadata,
        &section.metadata.richschema["ui"],
        &section.mount,
        &[],
        &base,
        &doc.patch,
        &mut out,
        0,
    )?;
    Ok(out)
}
fn collect(
    schema: &Value,
    metadata: &crate::metadata::Metadata,
    ui: &Value,
    mount: &[String],
    local: &[String],
    base: &Value,
    patch: &Value,
    out: &mut Vec<Field>,
    depth: usize,
) -> Result<(), String> {
    if depth > 16 || out.len() >= 256 {
        return Err("表单深度或字段数量超限".into());
    }
    let original_schema = schema;
    let resolved = resolve(schema, &metadata.richschema["schema"], 0)?;
    let schema = &resolved;
    let annotation = &ui["fields"][pointer(local)];
    if annotation["hidden"] == true {
        return Ok(());
    }
    if let Some(properties) = schema["properties"]
        .as_object()
        .filter(|_| annotation["widget"] != "color")
    {
        let mut entries: Vec<_> = properties.iter().collect();
        entries.sort_by_key(|(key, _)| {
            let mut path = local.to_vec();
            path.push((*key).clone());
            ui["fields"][pointer(&path)]["order"].as_i64().unwrap_or(0)
        });
        for (key, child) in entries {
            if key == "$schema" {
                continue;
            }
            let mut next = local.to_vec();
            next.push(key.clone());
            collect(
                child,
                metadata,
                ui,
                mount,
                &next,
                base,
                patch,
                out,
                depth + 1,
            )?;
        }
        return Ok(());
    }
    let mut path = mount.to_vec();
    path.extend_from_slice(local);
    let inherited = at(base, local).cloned().unwrap_or(Value::Null);
    let current = at(patch, &path);
    let color_input = annotation["widget"] == "color";
    let branches = schema["oneOf"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|item| resolve(item, &metadata.richschema["schema"], 0))
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    let presets = branches.iter().find_map(|branch| branch["enum"].as_array());
    let enum_number = !color_input
        && branches.len() == 2
        && presets.is_some()
        && branches
            .iter()
            .any(|branch| branch["type"] == "number" || branch["type"] == "integer");
    let custom_default = annotation["customDefault"]
        .as_f64()
        .or_else(|| {
            branches
                .iter()
                .find_map(|branch| branch["default"].as_f64())
        })
        .or_else(|| {
            branches
                .iter()
                .find_map(|branch| branch["minimum"].as_f64())
        })
        .unwrap_or(0.0)
        .to_string();
    let allow_system_color =
        color_input && metadata.accepts(original_schema, &Value::String("system".into()))?;
    let adaptive_color = color_input
        && metadata.accepts(
            original_schema,
            &serde_json::json!({"light":"#112233", "dark":"#445566"}),
        )?;
    let values = if enum_number {
        presets.cloned().unwrap_or_default()
    } else if schema["type"] == "boolean" {
        vec![Value::Bool(false), Value::Bool(true)]
    } else {
        schema["enum"].as_array().cloned().unwrap_or_default()
    };
    let kind = if color_input {
        "color"
    } else if enum_number {
        "enum_number"
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
    if kind == "choice" || enum_number {
        choices.extend(values.iter().map(display));
        if enum_number {
            choices.push("自定义".into());
        }
    } else {
        choices.push("覆盖".into());
    }
    let selected = match current {
        None => 0,
        Some(value) if enum_number => values
            .iter()
            .position(|v| v == value)
            .map(|i| i as i32 + 1)
            .unwrap_or(values.len() as i32 + 1),
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
    let inherited_selected = if enum_number {
        values
            .iter()
            .position(|v| v == &inherited)
            .map(|i| i as i32)
            .unwrap_or(values.len() as i32)
    } else {
        values_for_index(schema, &inherited)
    };
    out.push(Field {
        allow_system_color,
        adaptive_color,
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
        } else if enum_number {
            if value.is_number() {
                value.to_string()
            } else {
                custom_default.clone()
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
        } else if enum_number {
            if inherited.is_number() {
                inherited.to_string()
            } else {
                custom_default
            }
        } else if kind == "string" {
            display(&inherited)
        } else {
            inherited.to_string()
        },
        inherited_selected,
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
        if self.kind == "enum_number" && selected == self.values.len() as i32 + 1 {
            let size: f64 = text.trim().parse().map_err(|_| "请输入有效数值")?;
            if !size.is_finite() {
                return Err("请输入有限数值".into());
            }
            return Ok(Some(serde_json::json!(size)));
        }
        if self.kind == "choice" || self.kind == "enum_number" {
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

// 仅解析包内引用，限制引用链长度；循环和外部引用不能拖住表单生成。
fn resolve(schema: &Value, root: &Value, depth: usize) -> Result<Value, String> {
    if depth > 16 {
        return Err("表单 schema 引用深度超限".into());
    }
    let Some(reference) = schema.get("$ref").and_then(Value::as_str) else {
        return Ok(schema.clone());
    };
    let target = reference
        .strip_prefix('#')
        .and_then(|path| root.pointer(path))
        .ok_or("无效的本地 schema 引用")?;
    let mut result = resolve(target, root, depth + 1)?;
    if let (Some(output), Some(siblings)) = (result.as_object_mut(), schema.as_object()) {
        for (key, value) in siblings {
            if key != "$ref" {
                output.insert(key.clone(), value.clone());
            }
        }
    }
    Ok(result)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::Metadata;
    use serde_json::json;

    #[test]
    fn controls_follow_metadata_not_theme_paths() {
        // 任意挂载路径、字段名和预设数量；覆盖引用、单色/双色以及重置索引。
        let metadata = Metadata::from_parts(json!({"ink":"#123456", "scale":"tiny"}), json!({
            "formatVersion":1,
            "schema":{"type":"object", "$defs":{
                "hex":{"type":"string", "pattern":"^#[0-9a-fA-F]{6}$"},
                "paint":{"oneOf":[{"$ref":"#/$defs/hex"},{"type":"object",
                    "properties":{"light":{"$ref":"#/$defs/hex"},"dark":{"$ref":"#/$defs/hex"}},
                    "required":["light","dark"],"additionalProperties":false}]}
            }, "properties":{
                "ink":{"$ref":"#/$defs/paint", "title":"墨色"},
                "scale":{"oneOf":[{"enum":["tiny","huge"]},{"type":"number","minimum":2,"maximum":40}]}
            }},
            "ui":{"fields":{"/ink":{"widget":"color"},"/scale":{"customDefault":12}}}
        })).unwrap();
        let mount = vec!["arbitrary".into()];
        let mut fields = Vec::new();
        collect(
            &metadata.richschema["schema"],
            &metadata,
            &metadata.richschema["ui"],
            &mount,
            &[],
            &metadata.defaults,
            &json!({}),
            &mut fields,
            0,
        )
        .unwrap();
        let color = fields.iter().find(|f| f.key == "ink").unwrap();
        assert_eq!(color.kind, "color");
        assert_eq!(color.title, "墨色");
        assert!(color.adaptive_color);
        assert!(!color.allow_system_color);
        let scale = fields.iter().find(|f| f.key == "scale").unwrap();
        assert_eq!(scale.kind, "enum_number");
        assert_eq!(scale.text, "12");
        assert_eq!(scale.display_selected(), 0);
        assert_eq!(scale.value(3, "17").unwrap(), Some(json!(17.0)));
        assert_eq!(scale.value(0, "17").unwrap(), None);
        assert!(resolve(&json!({"$ref":"#"}), &json!({"$ref":"#"}), 0).is_err());
        // 内置描述与后加载的 WASM 描述走同一路径。
        for (defaults, rich, expected_system, expected_count) in [
            (
                include_str!("../../themes/eleven/src/config.json"),
                include_str!("../../themes/eleven/src/config.richschema.json"),
                true,
                3,
            ),
            (
                include_str!("../../themes/wasm/theme-weaselui/config.json"),
                include_str!("../../themes/wasm/theme-weaselui/config.richschema.json"),
                false,
                13,
            ),
        ] {
            let metadata = Metadata::from_parts(
                serde_json::from_str(defaults).unwrap(),
                serde_json::from_str(rich).unwrap(),
            )
            .unwrap();
            let mut fields = Vec::new();
            collect(
                &metadata.richschema["schema"],
                &metadata,
                &metadata.richschema["ui"],
                &mount,
                &[],
                &metadata.defaults,
                &json!({}),
                &mut fields,
                0,
            )
            .unwrap();
            let colors: Vec<_> = fields.iter().filter(|f| f.kind == "color").collect();
            assert_eq!(colors.len(), expected_count);
            assert!(
                colors
                    .iter()
                    .all(|f| f.adaptive_color && f.allow_system_color == expected_system)
            );
        }
    }
}
