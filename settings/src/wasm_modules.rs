//! 模块列表仅编辑配置，不读取新路径或执行 WASM。
use crate::{SettingsWindow, State, WasmEntry, form};
use serde_json::{Value, json};
use slint::{Model, VecModel};
use std::{collections::BTreeSet, rc::Rc};

fn path(id: &str) -> Vec<String> {
    vec![
        "themeSettings".into(),
        "wasm".into(),
        "modules".into(),
        id.into(),
    ]
}
pub fn show(state: &State, ui: &SettingsWindow) -> Result<(), String> {
    let effective = state.document.effective();
    let wasm = &effective["themeSettings"]["wasm"];
    let modules = &wasm["modules"];
    if !modules.is_null() && !modules.is_object() {
        return Err("modules 必须为对象".into());
    }
    let ids: BTreeSet<_> = modules
        .as_object()
        .into_iter()
        .flat_map(|m| m.keys().cloned())
        .collect();
    if ids.len() > 32 {
        return Err("WASM 模块最多 32 个".into());
    }
    let old = ui.get_wasm_modules();
    let mut rows = Vec::new();
    for id in ids {
        let item = &modules[&id];
        if !item.is_object() {
            return Err(format!("模块 {id} 必须为对象"));
        }
        let string = |key| -> Result<String, String> {
            match item.get(key) {
                None => Ok(String::new()),
                Some(Value::String(s)) => Ok(s.clone()),
                _ => Err(format!("{id}.{key} 必须为字符串")),
            }
        };
        let patch = form::at(&state.document.patch, &path(&id));
        let expanded = (0..old.row_count())
            .filter_map(|i| old.row_data(i))
            .any(|r| r.id.as_str() == id && r.expanded);
        rows.push(WasmEntry {
            id: id.clone().into(),
            name: string("name")?.into(),
            file: string("file")?.into(),
            expanded,
            active: wasm["theme"].as_str() == Some(&id),
            overridden: patch.is_some(),
            name_overridden: patch.and_then(|p| p.get("name")).is_some(),
            file_overridden: patch.and_then(|p| p.get("file")).is_some(),
        });
    }
    // 字段输入时保持模型和控件实例，避免每个字符重建输入框而丢失焦点。
    if old.row_count() == rows.len()
        && rows
            .iter()
            .enumerate()
            .all(|(i, r)| old.row_data(i).is_some_and(|o| o.id == r.id))
    {
        for (i, row) in rows.into_iter().enumerate() {
            old.set_row_data(i, row);
        }
    } else {
        ui.set_wasm_modules(Rc::new(VecModel::from(rows)).into());
    }
    Ok(())
}
pub fn action(
    state: &mut State,
    kind: &str,
    id: &str,
    key: &str,
    value: &str,
) -> Result<(), String> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err("模块标识仅允许字母、数字、下划线和连字符，最多 128 字符".into());
    }
    let effective = state.document.effective();
    let modules = &effective["themeSettings"]["wasm"]["modules"];
    let mut target = path(id);
    match kind {
        "add" => {
            if modules
                .as_object()
                .is_some_and(|m| m.keys().any(|k| k.eq_ignore_ascii_case(id)))
            {
                return Err("模块标识已存在".into());
            }
            if modules.as_object().is_some_and(|m| m.len() >= 32) {
                return Err("WASM 模块最多 32 个".into());
            }
            form::assign(&mut state.document.patch, &target, Some(json!({})))?;
        }
        "remove" => {
            form::assign(&mut state.document.patch, &target, None)?;
        }
        "select" => {
            if modules.get(id).is_none() {
                return Err("模块不存在".into());
            }
            form::assign(
                &mut state.document.patch,
                &["themeSettings".into(), "wasm".into(), "theme".into()],
                Some(json!(id)),
            )?;
        }
        "set" | "reset" => {
            if modules.get(id).is_none() || !["name", "file"].contains(&key) {
                return Err("无效模块字段".into());
            }
            if kind == "set" {
                if value.len() > 4096 || value.chars().any(char::is_control) {
                    return Err("字段过长或包含控制字符".into());
                }
                if key == "file" && value.replace('/', "\\").starts_with("\\\\") {
                    return Err("模块路径不允许网络或设备路径".into());
                }
            }
            target.push(key.into());
            form::assign(
                &mut state.document.patch,
                &target,
                if kind == "reset" {
                    None
                } else {
                    Some(json!(value))
                },
            )?;
        }
        _ => return Err("无效模块操作".into()),
    }
    Ok(())
}
