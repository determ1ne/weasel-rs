//! 应用项只修改指定字段；未知键保留，清除覆盖后仍显示安装配置中的条目。
use crate::{config::Document, form};
use serde_json::{Value, json};

const KEYS: [&str; 2] = ["ascii_mode", "inline_preedit"];
pub fn edit(doc: &mut Document, app: &str, key: &str, value: Option<bool>) -> Result<(), String> {
    if !KEYS.contains(&key) {
        return Err("不支持的应用选项".into());
    }
    if doc.effective()["app_options"].get(app).is_none() {
        return Err("应用项不存在".into());
    }
    form::assign(
        &mut doc.patch,
        &["app_options".into(), app.into(), key.into()],
        value.map(Value::Bool),
    )
}
pub fn add(doc: &mut Document, app: &str) -> Result<(), String> {
    let app = app.trim();
    if app.is_empty()
        || app.len() > 255
        || !app.to_ascii_lowercase().ends_with(".exe")
        || app
            .chars()
            .any(|c| c.is_control() || "<>:\"/\\|?*".contains(c))
    {
        return Err("请输入程序文件名，例如 notepad.exe，不要包含路径".into());
    }
    let effective = doc.effective();
    if let Some(items) = effective["app_options"].as_object() {
        if items.len() >= 256 {
            return Err("应用项最多 256 个".into());
        }
        if items.keys().any(|k| k.eq_ignore_ascii_case(app)) {
            return Err("该程序已存在".into());
        }
    } else if !effective["app_options"].is_null() {
        return Err("app_options 必须为对象，请先修复配置文件".into());
    }
    form::assign(
        &mut doc.patch,
        &["app_options".into(), app.into()],
        Some(json!({})),
    )
}
pub fn remove(doc: &mut Document, app: &str) -> Result<(), String> {
    form::assign(&mut doc.patch, &["app_options".into(), app.into()], None)
}
