//! 管理按应用区分的输入选项覆盖，支持编辑、新增和删除应用项。
//! 操作只针对受支持字段；未知键保留，清除覆盖后仍可继承安装配置中的条目。
use crate::{config::Document, form};
use serde_json::{Value, json};

const KEYS: [&str; 2] = ["ascii_mode", "inline_preedit"];
/// 更新已有应用的受支持选项；传入 `None` 会移除该字段的覆盖。
///
/// 未知选项或有效配置中不存在该应用时返回错误，其他应用字段和未知键保持不变。
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
/// 在覆盖配置中新增应用项；应用标识必须是安全的 `.exe` 文件名而非路径。
///
/// 校验基于合并后的有效配置，应用名按 ASCII 大小写不敏感比较，最多允许 256 项。
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
/// 从覆盖配置中删除应用项；删除继承项的覆盖后，该项会重新显示安装配置中的值。
pub fn remove(doc: &mut Document, app: &str) -> Result<(), String> {
    form::assign(&mut doc.patch, &["app_options".into(), app.into()], None)
}
