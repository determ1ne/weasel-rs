//! WASM 主题入口：从配置定位 .wasm 文件，装配 canvas + runtime + window，
//! 产出 ThemeBackend。加载/实例化失败时返回 `Err`，renderer 走既有回退路径。

use crate::backend::WasmBackend;
use crate::runtime::{HostState, WasmRuntime};
use crate::theme_api::{ThemeBackend, ThemeCapabilities, ThemeCreation, ThemeFactory, UiMode};
use crate::window::{Window, create_window};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use weasel_common::settings::ConfigSnapshot;

pub struct Factory;

/// Default search is installation first, user data second; explicit paths do not fall back.
fn module_paths(
    id: &str,
    explicit: Option<&str>,
    program: &std::path::Path,
    user: &std::path::Path,
) -> Vec<PathBuf> {
    match explicit.filter(|path| !path.is_empty()) {
        Some(path) => vec![user.join(path)],
        None => vec![
            program.join("theme-wasm").join(format!("{id}.wasm")),
            user.join(format!("{id}.wasm")),
        ],
    }
}

fn load_theme(paths: &[PathBuf]) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut selected = None;
    for path in paths {
        match std::fs::File::open(path) {
            Ok(file) => {
                selected = Some((path, file));
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("failed to open WASM theme {}: {e}", path.display())),
        }
    }
    let (path, file) = selected.ok_or_else(|| {
        format!(
            "WASM theme not found; tried: {}",
            paths
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )
    })?;
    let mut bytes = Vec::new();
    file.take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    if bytes.len() > 16 * 1024 * 1024 {
        return Err("WASM module exceeds 16 MiB".into());
    }
    Ok(bytes)
}

impl ThemeFactory for Factory {
    fn name(&self) -> &'static str {
        "wasm"
    }

    fn capabilities(&self) -> ThemeCapabilities {
        // The guest probes the actual requirement during creation.
        ThemeCapabilities { preedit: true }
    }

    fn create(&self, mode: UiMode, settings: &ConfigSnapshot) -> ThemeCreation {
        (|| -> Result<Box<dyn ThemeBackend>, String> {
            if windows_version::OsVersion::current()
                < windows_version::OsVersion::new(10, 0, 0, 17134)
            {
                return Err(
                    "WASM composition themes require Windows 10 version 1803 or newer".into(),
                );
            }
            let wasm_settings: serde_json::Map<String, serde_json::Value> =
                settings.theme_settings("wasm")?;
            let theme = match wasm_settings.get("theme") {
                None => "weaselui",
                Some(serde_json::Value::String(value)) => value.as_str(),
                _ => return Err("themeSettings.wasm.theme must be a string".into()),
            };
            if theme.is_empty()
                || !theme
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
            {
                return Err("themeSettings.wasm.theme must be a module basename".into());
            }
            let modules = match wasm_settings.get("modules") {
                None => serde_json::Map::new(),
                Some(serde_json::Value::Object(modules)) => modules.clone(),
                _ => return Err("themeSettings.wasm.modules must be an object".into()),
            };
            let entry = modules
                .get(theme)
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            if !entry.is_object() || entry.get("name").is_some_and(|v| !v.is_string()) {
                return Err(format!(
                    "themeSettings.wasm.modules.{theme} must be an object with optional string name"
                ));
            }
            let explicit = match entry.get("file").or_else(|| entry.get("path")) {
                None => None,
                Some(serde_json::Value::String(path)) => Some(path.as_str()),
                _ => {
                    return Err(format!(
                        "themeSettings.wasm.modules.{theme}.file must be a string"
                    ));
                }
            };
            let paths = weasel_common::runtime_paths::RuntimePaths::discover()
                .map_err(|e| e.to_string())?;
            let candidates = module_paths(
                theme,
                explicit,
                &paths.executable_directory,
                &paths.user_data,
            );
            let bytes = load_theme(&candidates)?;
            // canvas 先于 runtime：measure_text 导入委托给 canvas 的 DirectWrite 实测。
            let canvas = crate::canvas::Canvas::new().map_err(|e| e.to_string())?;
            let canvas = Rc::new(RefCell::new(canvas));
            let measurer = Rc::clone(&canvas);
            let options = entry
                .get("config")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            if !options.is_object() {
                return Err(format!(
                    "themeSettings.wasm.modules.{theme}.config must be an object"
                ));
            }
            let mut state = HostState::default();
            state.options = options;
            // Expose only supported presentation settings, separate from guest config.
            state.settings = serde_json::json!({
                "preedit_type": settings.query(".preedit_type")?.cloned()
                    .unwrap_or_else(|| serde_json::json!("composition")),
            });
            let font_canvas = measurer.clone();
            state.set_font =
                Box::new(move |slot, family| font_canvas.borrow_mut().set_font(slot, family));
            let metric_canvas = measurer.clone();
            state.line_height = Box::new(move |slot, size| {
                metric_canvas
                    .borrow_mut()
                    .line_height(slot, size)
                    .unwrap_or(size * 1.4)
            });
            state.measure = Box::new(move |text, font, size| {
                measurer
                    .borrow_mut()
                    .measure(font, size, text)
                    .unwrap_or(0.0)
            });
            let mut runtime = WasmRuntime::with_state(&bytes, state)
                .map_err(|e| format!("failed to instantiate WASM theme: {e}"))?;
            runtime.configure(settings.needs_external_preedit())?;
            // Fail during factory creation so renderer can select a fallback.
            runtime.init(
                if mode == UiMode::Live {
                    crate::protocol::MODE_LIVE
                } else {
                    crate::protocol::MODE_PREVIEW
                },
                crate::appearance::is_dark(),
            )?;
            let window = Rc::new(Window::new(canvas, runtime, mode != UiMode::Live));
            create_window(&window)?;
            window.health()?;
            Ok(Box::new(WasmBackend::new(window)))
        })()
        .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn module_search_order_and_explicit_override() {
        let program = std::path::Path::new(r"C:\Weasel");
        let user = std::path::Path::new(r"C:\Users\example\Rime");
        for explicit in [None, Some("")] {
            assert_eq!(
                module_paths("weaselui", explicit, program, user),
                vec![
                    program.join("theme-wasm/weaselui.wasm"),
                    user.join("weaselui.wasm")
                ]
            );
        }
        assert_eq!(
            module_paths("weaselui", Some("custom.wasm"), program, user),
            vec![user.join("custom.wasm")]
        );
        assert_eq!(
            module_paths("weaselui", Some(r"D:\custom.wasm"), program, user),
            vec![PathBuf::from(r"D:\custom.wasm")]
        );
    }
}
