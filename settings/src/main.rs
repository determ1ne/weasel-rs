//! 配置描述仅启动时读取；主题选择只改变单页分区显隐。
#![windows_subsystem = "windows"]
mod app_options;
mod backdrop;
mod bindings;
mod broker;
mod catalog;
mod colors;
mod config;
mod form;
mod frame;
mod metadata;
mod updater;
mod wasm_modules;
use slint::{ComponentHandle, Model, VecModel};
use std::{cell::RefCell, collections::HashSet, rc::Rc};
use weasel_common::process::RuntimePaths;
slint::include_modules!();

/// 设置窗口的运行时模型；仅在 Slint 事件循环线程内访问。
///
/// `document` 保存基础配置与用户覆盖，`fields`/`owners` 将 UI 行映射回目录分区；
/// `changed` 用于限定保存前的分区校验，`invalid` 和 `module_errors` 阻止保存。
struct State {
    /// 已加载的配置文档，包含基础值及待保存的用户补丁。
    document: config::Document,
    /// 配置项目录及其分区元数据。
    catalog: catalog::Catalog,
    /// 与完整 UI 行模型按索引对应的编辑字段。
    fields: Vec<form::Field>,
    /// 每个字段所属的目录分区索引，与 `fields` 同步增长。
    owners: Vec<usize>,
    /// 自上次成功保存以来发生改动的分区索引。
    changed: HashSet<usize>,
    /// 当前无效的字段索引；存在时禁止保存。
    invalid: HashSet<usize>,
    /// 以“模块 ID/键”标识的无效模块编辑。
    module_errors: HashSet<String>,
    /// 最近成功读取的自动更新设置；读取失败时为 `None`，此项不可编辑。
    auto_update_saved: Option<bool>,
    /// 界面当前选择的自动更新值，成功写入后同步到 `auto_update_saved`。
    auto_update_pending: bool,
}
impl State {
    /// 根据有效配置重建应用选项行，并保留已有行的展开状态。
    ///
    /// 遇到非对象配置、超过 256 项或无效布尔值时返回错误，不提交部分模型。
    fn show_apps(&self, ui: &SettingsWindow) -> Result<(), String> {
        let effective = self.document.effective();
        let snapshot = weasel_common::settings::ConfigSnapshot::new(effective.clone());
        let Some(items) = effective["app_options"].as_object() else {
            if effective["app_options"].is_null() {
                ui.set_apps(Default::default());
                return Ok(());
            }
            return Err("app_options 必须为对象".into());
        };
        if items.len() > 256 {
            return Err("应用项最多 256 个".into());
        }
        let old = ui.get_apps();
        let expanded: HashSet<String> = (0..old.row_count())
            .filter_map(|i| old.row_data(i))
            .filter(|r| r.expanded)
            .map(|r| r.name.to_string())
            .collect();
        let mut rows = Vec::new();
        for (name, config) in items {
            if !config.is_object() {
                return Err(format!("app_options.{name} 必须为对象"));
            }
            let patch = &self.document.patch["app_options"][name];
            let mut options = Vec::new();
            for (key, title) in [
                ("ascii_mode", "默认英文模式"),
                ("inline_preedit", "内联预编辑"),
            ] {
                let checked = snapshot
                    .app_bool(name, key)
                    .map_err(|error| format!("{name}.{key}: {error}"))?;
                options.push(AppOption {
                    key: key.into(),
                    title: title.into(),
                    checked,
                    overridden: patch.get(key).is_some(),
                    description: format!(
                        "{}\n{}",
                        if patch.get(key).is_some() {
                            "用户指定；恢复继承会清除此覆盖。"
                        } else if config.get(key).is_some() {
                            "程序配置已指定此应用选项。"
                        } else {
                            "未指定：继承全局设置，随全局设置变化。"
                        },
                        if key == "ascii_mode" {
                            "首次激活时使用英文模式；启用共享中英文状态时以共享状态为准。"
                        } else {
                            "在应用文本框内显示预编辑；关闭时由支持此功能的主题显示。"
                        }
                    )
                    .into(),
                });
            }
            rows.push(AppEntry {
                name: name.clone().into(),
                expanded: expanded.contains(name),
                overridden: self.document.patch["app_options"].get(name).is_some(),
                options: Rc::new(VecModel::from(options)).into(),
            });
        }
        ui.set_apps(Rc::new(VecModel::from(rows)).into());
        Ok(())
    }
    /// 判断目录分区是否匹配当前主题及可选的 WASM 主题挂载条件。
    fn visible(&self, index: usize) -> bool {
        let value = self.document.effective();
        let mount = &self.catalog.sections[index].mount;
        mount.is_empty()
            || (value["theme"].as_str() == mount.get(1).map(String::as_str)
                && (mount.len() < 5
                    || value["themeSettings"]["wasm"]["theme"].as_str()
                        == mount.get(3).map(String::as_str)))
    }
    /// 按目录生成完整字段模型和过滤后的可见模型，并同步应用及 WASM 模块行。
    ///
    /// 字段、所有者映射与 UI 源索引保持同序。此方法会向字段和所有者列表追加内容，
    /// 因此应在新建状态时调用；模型生成失败时返回错误。
    fn show(&mut self, ui: &SettingsWindow) -> Result<(), String> {
        let mut rows = Vec::new();
        for (index, section) in self.catalog.sections.iter().enumerate() {
            for (offset, field) in form::fields(section, &self.document)?
                .into_iter()
                .enumerate()
            {
                rows.push(SettingField {
                    source_index: self.fields.len() as i32,
                    section: if offset == 0 {
                        section.title.clone().into()
                    } else {
                        "".into()
                    },
                    shown: self.visible(index),
                    title: field.title.clone().into(),
                    key: field.key.clone().into(),
                    description: field.description.clone().into(),
                    choices: Rc::new(VecModel::from(
                        field
                            .choices
                            .iter()
                            .skip(1)
                            .map(|s| s.clone().into())
                            .collect::<Vec<_>>(),
                    ))
                    .into(),
                    selected: field.display_selected(),
                    checked: field.display_selected() == 1,
                    overridden: field.selected != 0,
                    boolean: field.boolean,
                    text: field.text.clone().into(),
                    text_input: field.kind != "choice" && field.kind != "enum_number",
                    enum_number: field.kind == "enum_number",
                    color_input: field.kind == "color",
                    allow_system_color: field.allow_system_color,
                    adaptive_color: field.adaptive_color,
                    multiline: field.multiline,
                });
                self.fields.push(field);
                self.owners.push(index);
            }
        }
        let model = Rc::new(VecModel::from(rows));
        // 不向布局传递隐藏行，避免空 VerticalBox 的默认 padding/spacing 累积。
        // 保留源索引，过滤后的行号不能用作编辑草稿的索引。
        ui.set_visible_fields(
            Rc::new(slint::FilterModel::new(
                model.clone(),
                |row: &SettingField| row.shown,
            ))
            .into(),
        );
        ui.set_fields(model.into());
        self.show_apps(ui)?;
        wasm_modules::show(self, ui)?;
        Ok(())
    }
    /// 原位更新字段行的可见标志，不重建模型或改变编辑索引。
    fn update_visibility(&self, ui: &SettingsWindow) {
        let model = ui.get_fields();
        for (index, &owner) in self.owners.iter().enumerate() {
            if let Some(mut row) = model.row_data(index) {
                let shown = self.visible(owner);
                if row.shown != shown {
                    row.shown = shown;
                    model.set_row_data(index, row);
                }
            }
        }
    }
}
/// 初始化原生窗口后启动 Slint 事件循环；配置目录在后台线程读取。
///
/// 所有 UI 状态和回调均留在事件循环线程，后台任务通过容量为 1 的通道回传结果。
/// 返回窗口初始化、线程创建或事件循环产生的错误。
fn main() -> Result<(), Box<dyn std::error::Error>> {
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("skia".into())
        .with_winit_window_attributes_hook(|attributes| {
            use slint::winit_030::winit::platform::windows::{
                BackdropType, WindowAttributesExtWindows,
            };
            // 在 Winit 创建 HWND 时设置材质，早于渲染表面初始化。
            attributes
                .with_enabled_buttons(
                    slint::winit_030::winit::window::WindowButtons::MINIMIZE
                        | slint::winit_030::winit::window::WindowButtons::CLOSE,
                )
                .with_transparent(backdrop::supported())
                .with_system_backdrop(if backdrop::supported() {
                    BackdropType::MainWindow
                } else {
                    BackdropType::None
                })
        })
        .select()?;
    let window = SettingsWindow::new()?;
    // 首帧就以透明背景初始化渲染表面；apply 失败时恢复纯色。
    window.set_mica_enabled(backdrop::supported());
    colors::install(&window);
    let paths = RuntimePaths::discover()?;
    window.set_config_path(
        paths
            .user_data
            .join("weasel.custom.json")
            .display()
            .to_string()
            .into(),
    );
    let state = Rc::new(RefCell::new(None::<State>));
    {
        let state = state.clone();
        let weak = window.as_weak();
        window.on_changed(move |index, selected, text| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let mut guard = state.borrow_mut();
            let Some(state) = guard.as_mut() else {
                return;
            };
            let Some(field) = state.fields.get_mut(index as usize) else {
                return;
            };
            field.selected = selected;
            field.text = text.to_string();
            let result = field
                .value(selected, text.as_str())
                .and_then(|value| form::assign(&mut state.document.patch, &field.path, value));
            if selected == 0 && result.is_ok() {
                field.text = field.inherited_text.clone();
            }
            state.changed.insert(state.owners[index as usize]);
            let model = ui.get_fields();
            if let Some(mut row) = model.row_data(index as usize) {
                row.selected = field.display_selected();
                row.checked = field.display_selected() == 1;
                row.overridden = selected != 0;
                row.text = field.text.clone().into();
                model.set_row_data(index as usize, row);
            }
            ui.set_dirty(true);
            match result {
                Ok(()) => {
                    state.invalid.remove(&(index as usize));
                    state.update_visibility(&ui);
                    if let Err(error) = state.show_apps(&ui) {
                        ui.set_status(error.into());
                    }
                    if let Err(error) = wasm_modules::show(state, &ui) {
                        ui.set_status(error.into());
                    }
                    ui.set_status("有未保存修改；可仅保存或保存并应用。".into());
                }
                Err(error) => {
                    state.invalid.insert(index as usize);
                    ui.set_status(format!("字段无效：{error}").into());
                }
            }
            ui.set_invalid(!state.invalid.is_empty() || !state.module_errors.is_empty());
        });
    }
    {
        let state = state.clone();
        let weak = window.as_weak();
        window.on_update_changed(move |checked| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let mut guard = state.borrow_mut();
            let Some(state) = guard.as_mut() else {
                return;
            };
            if let Some(saved) = state.auto_update_saved {
                state.auto_update_pending = checked;
                let dirty = !state.changed.is_empty() || checked != saved;
                ui.set_dirty(dirty);
                ui.set_status(if dirty {
                    "有未保存修改；可仅保存或保存并应用。".into()
                } else {
                    "自动检查更新设置未更改。".into()
                });
            }
        });
    }
    {
        let state = state.clone();
        let weak = window.as_weak();
        let update_directory = paths.executable_directory.clone();
        window.on_save(move |apply| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let mut guard = state.borrow_mut();
            let Some(state) = guard.as_mut() else {
                return;
            };
            if !state.invalid.is_empty() || !state.module_errors.is_empty() {
                return;
            }
            for &index in &state.changed {
                let section = &state.catalog.sections[index];
                if let Err(error) = section
                    .metadata
                    .validate(&form::effective(section, &state.document))
                {
                    ui.set_status(format!("{}：{error}", section.title).into());
                    return;
                }
            }
            let config_changed = !state.changed.is_empty();
            let update_changed = state
                .auto_update_saved
                .is_some_and(|saved| saved != state.auto_update_pending);
            if config_changed {
                if let Err(error) = state.document.save() {
                    ui.set_status(format!("保存失败：{error}").into());
                    return;
                }
                state.changed.clear();
            }
            if update_changed {
                if let Err(error) = updater::write(&update_directory, state.auto_update_pending) {
                    ui.set_dirty(true);
                    let prefix = if config_changed {
                        "普通配置已保存，但"
                    } else {
                        ""
                    };
                    ui.set_status(format!("{prefix}自动检查更新设置保存失败：{error}").into());
                    return;
                }
                state.auto_update_saved = Some(state.auto_update_pending);
            }
            ui.set_dirty(false);
            let mut messages = Vec::new();
            // 现有重启命令只重启子组件，不重新初始化 broker 中的 WinSparkle。
            if apply && (config_changed || !update_changed) {
                messages.push(match broker::request_restart() {
                    Ok(()) => "已向 broker 发送重启服务请求。".to_owned(),
                    Err(error) => format!("未能请求应用普通配置：{error}"),
                });
            } else if config_changed {
                messages.push("普通配置已保存，尚未请求应用。".to_owned());
            }
            if update_changed {
                messages.push(
                    "自动检查更新设置已保存；退出并重新启动托盘中的算法服务后生效。".to_owned(),
                );
            }
            if messages.is_empty() {
                messages.push("没有需要保存的修改。".to_owned());
            }
            ui.set_status(messages.join("\n").into());
        });
    }
    window.set_status("正在读取配置描述…".into());
    {
        let state = state.clone();
        let weak = window.as_weak();
        window.on_module_action(move |kind, id, key, value| {
            let Some(ui) = weak.upgrade() else {
                return "设置窗口不可用".into();
            };
            let mut guard = state.borrow_mut();
            let Some(state) = guard.as_mut() else {
                return "配置尚未加载".into();
            };
            let error_key = format!("{id}/{key}");
            if let Err(error) = wasm_modules::action(state, &kind, &id, &key, &value) {
                if kind == "set" {
                    state.module_errors.insert(error_key);
                    ui.set_invalid(true);
                }
                return error.into();
            }
            if kind == "remove" {
                state
                    .module_errors
                    .retain(|k| !k.starts_with(&format!("{id}/")));
            } else {
                state.module_errors.remove(&error_key);
            }
            ui.set_invalid(!state.invalid.is_empty() || !state.module_errors.is_empty());
            state.changed.insert(0);
            ui.set_dirty(true);
            // 使用模块同时同步原有 theme 字段及主题分区，不重新读取描述。
            let effective = state.document.effective();
            for (i, field) in state.fields.iter_mut().enumerate() {
                if kind == "remove"
                    && field.path.len() >= 5
                    && field.path[..3] == ["themeSettings", "wasm", "modules"]
                    && field.path[3] == id.as_str()
                {
                    field.selected = 0;
                    field.text = field.inherited_text.clone();
                    state.invalid.remove(&i);
                    if let Some(mut row) = ui.get_fields().row_data(i) {
                        row.text = field.text.clone().into();
                        row.selected = field.display_selected();
                        row.checked = field.display_selected() == 1;
                        row.overridden = false;
                        ui.get_fields().set_row_data(i, row);
                    }
                }
                if kind == "select" && field.path == ["themeSettings", "wasm", "theme"] {
                    field.text = effective["themeSettings"]["wasm"]["theme"]
                        .as_str()
                        .unwrap_or("")
                        .into();
                    field.selected = 1;
                    if let Some(mut row) = ui.get_fields().row_data(i) {
                        row.text = field.text.clone().into();
                        row.overridden = true;
                        ui.get_fields().set_row_data(i, row);
                    }
                }
            }
            state.update_visibility(&ui);
            ui.set_invalid(!state.invalid.is_empty() || !state.module_errors.is_empty());
            if let Err(error) = wasm_modules::show(state, &ui) {
                return error.into();
            }
            ui.set_status("有未保存修改；新增模块或变更路径后需重新打开设置加载描述。".into());
            "".into()
        });
    }
    {
        let state = state.clone();
        let weak = window.as_weak();
        window.on_app_add(move |name| {
            let Some(ui) = weak.upgrade() else {
                return false;
            };
            if let Some(state) = state.borrow_mut().as_mut() {
                let result = app_options::add(&mut state.document, name.as_str());
                if let Err(error) = result {
                    ui.set_app_add_error(error.into());
                    return false;
                }
                ui.set_app_add_error("".into());
                let added = true;
                finish_app_edit(state, &ui, Ok(()));
                if added {
                    let model = ui.get_apps();
                    for i in 0..model.row_count() {
                        if let Some(mut row) = model.row_data(i) {
                            if row.name.as_str() == name.trim() {
                                row.expanded = true;
                                model.set_row_data(i, row);
                                break;
                            }
                        }
                    }
                }
                return added;
            }
            false
        });
    }
    {
        let state = state.clone();
        let weak = window.as_weak();
        window.on_app_remove(move |name| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            if let Some(state) = state.borrow_mut().as_mut() {
                let result = app_options::remove(&mut state.document, name.as_str());
                finish_app_edit(state, &ui, result);
            }
        });
    }
    {
        let state = state.clone();
        let weak = window.as_weak();
        window.on_app_edit(move |name, key, value, reset| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            if let Some(state) = state.borrow_mut().as_mut() {
                let result = app_options::edit(
                    &mut state.document,
                    name.as_str(),
                    key.as_str(),
                    if reset { None } else { Some(value) },
                );
                finish_app_edit(state, &ui, result);
            }
        });
    }
    let weak = window.as_weak();
    let (tx, rx) = std::sync::mpsc::sync_channel::<
        Result<(config::Document, catalog::Catalog, Result<bool, String>), String>,
    >(1);
    window.on_loaded(move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let Ok(result) = rx.try_recv() else {
            return;
        };
        match result {
            Ok((document, catalog, auto_update)) => {
                let (auto_update_saved, update_status) = match auto_update {
                    Ok(enabled) => (Some(enabled), String::new()),
                    Err(error) => (None, format!("更新组件不可用：{error}")),
                };
                let mut next = State {
                    document,
                    catalog,
                    fields: vec![],
                    owners: vec![],
                    changed: HashSet::new(),
                    invalid: HashSet::new(),
                    module_errors: HashSet::new(),
                    auto_update_saved,
                    auto_update_pending: auto_update_saved.unwrap_or(false),
                };
                if let Err(error) = next.show(&ui) {
                    ui.set_status(error.into());
                    return;
                }
                ui.set_status(next.catalog.notices.join("\n").into());
                ui.set_update_available(auto_update_saved.is_some());
                ui.set_update_checked(next.auto_update_pending);
                ui.set_update_status(update_status.into());
                *state.borrow_mut() = Some(next);
                ui.set_editable(true);
            }
            Err(error) => ui.set_status(format!("加载失败：{error}").into()),
        }
    });
    let weak = window.as_weak();
    if let Err(error) = std::thread::Builder::new()
        .name("settings-catalog".into())
        .spawn(move || {
            let result = config::Document::load(&paths).and_then(|document| {
                catalog::load(&paths, &document).map(|catalog| {
                    let auto_update = updater::read(&paths.executable_directory);
                    (document, catalog, auto_update)
                })
            });
            if tx.send(result).is_ok() {
                let _ = weak.upgrade_in_event_loop(|ui| ui.invoke_loaded());
            }
        })
    {
        window.set_status(format!("无法启动读取任务：{error}").into());
    }
    window.on_request_close(|| {
        let _ = slint::quit_event_loop();
    });
    window.show()?;
    backdrop::apply(&window);
    slint::run_event_loop()?;
    Ok(())
}

/// 收尾应用选项编辑：成功时标记配置已修改并刷新行，失败时展示原始错误。
fn finish_app_edit(state: &mut State, ui: &SettingsWindow, result: Result<(), String>) {
    match result {
        Ok(()) => {
            state.changed.insert(0);
            ui.set_dirty(true);
            match state.show_apps(ui) {
                Ok(()) => ui.set_status(
                    "有未保存修改。清除应用覆盖后，安装配置中的应用项仍会保留。".into(),
                ),
                Err(error) => ui.set_status(error.into()),
            }
        }
        Err(error) => ui.set_status(error.into()),
    }
}
