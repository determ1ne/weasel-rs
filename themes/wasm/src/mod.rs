//! WASM 主题工厂：按配置定位并加载主题模块，创建画布、运行时与窗口后端。
//!
//! 模块加载、实例化、初始化或窗口健康检查失败都会返回错误，由 renderer
//! 沿现有主题回退路径处理。显式文件路径只尝试指定位置；未指定时先查安装
//! 目录，再查用户数据目录。

use crate::backend::WasmBackend;
use crate::runtime::{HostState, WasmRuntime};
use crate::theme_api::{ThemeBackend, ThemeCapabilities, ThemeCreation, ThemeFactory, UiMode};
use crate::window::{Window, create_window};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use weasel_common::settings::ConfigSnapshot;

/// 构造 WASM 主题后端的工厂。
pub struct Factory;

/// 计算模块候选路径；默认依次查安装目录和用户数据目录，显式路径仅生成一个候选。
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

/// 按候选顺序读取首个可打开的模块，并返回其字节和同名资源目录。
///
/// 缺失文件会继续尝试后续候选，其他打开错误立即返回；读取最多接受 16 MiB，
/// 超限或读取失败均返回描述性错误。该函数只读取文件，不实例化 WASM。
fn load_theme(paths: &[PathBuf]) -> Result<(Vec<u8>, PathBuf), String> {
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
    Ok((bytes, path.with_extension("assets")))
}

impl ThemeFactory for Factory {
    fn name(&self) -> &'static str {
        "wasm"
    }

    fn capabilities(&self) -> ThemeCapabilities {
        // The guest probes the actual requirement during creation.
        ThemeCapabilities {
            preedit: true,
            // Individual guests opt in with `theme_capabilities`; declaring the
            // superset here lets renderer deliver mode-only snapshots.
            resident: true,
            mode_indicator: true,
        }
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
            let paths =
                weasel_common::process::RuntimePaths::discover().map_err(|e| e.to_string())?;
            let candidates = module_paths(
                theme,
                explicit,
                &paths.executable_directory,
                &paths.user_data,
            );
            let (bytes, asset_root) = load_theme(&candidates)?;
            // canvas负责回放，runtime持有独立字体和布局资源。
            let canvas = crate::canvas::Canvas::new().map_err(|e| e.to_string())?;
            let canvas = Rc::new(RefCell::new(canvas));
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
            state.resources.asset_root = Some(asset_root);
            state.options = options;
            // Expose only supported presentation settings, separate from guest config.
            state.settings = serde_json::json!({
                "preedit_type": settings.required::<serde_json::Value>(".preedit_type")?,
            });
            let mut runtime = WasmRuntime::with_state(&bytes, state)
                .map_err(|e| format!("failed to instantiate WASM theme: {e}"))?;
            runtime.configure(settings.needs_external_preedit()?)?;
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
