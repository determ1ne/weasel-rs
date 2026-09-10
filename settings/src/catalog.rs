//! 主题配置目录。只读 JSON 和 WASM custom section，不加载 DLL、不实例化 WASM。
use crate::{config::Document, metadata::Metadata};
use std::{collections::BTreeSet, fs, io::Read, path::Path};
use weasel_common::runtime_paths::RuntimePaths;

pub struct Section {
    pub title: String,
    pub mount: Vec<String>,
    pub metadata: Metadata,
}
pub struct Catalog {
    pub sections: Vec<Section>,
    pub notices: Vec<String>,
}

// 启动加载有总读取预算，避免大量合法大小的模块造成无界限内存和 I/O 开销。
fn read(path: &Path, limit: usize, budget: &mut usize) -> Result<Vec<u8>, String> {
    local_path(path)?;
    let resolved = path
        .canonicalize()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    local_path(&resolved)?;
    let mut data = Vec::new();
    let file = fs::File::open(&resolved).map_err(|e| e.to_string())?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("主题描述必须是普通文件".into());
    }
    file.take((limit + 1) as u64)
        .read_to_end(&mut data)
        .map_err(|e| e.to_string())?;
    if data.len() > limit || data.len() > *budget {
        return Err("主题目录读取预算超限".into());
    }
    *budget -= data.len();
    Ok(data)
}
fn local_path(path: &Path) -> Result<(), String> {
    let text = path.to_string_lossy().replace('/', "\\");
    if text.starts_with("\\\\")
        && !(text.starts_with("\\\\?\\") && text.as_bytes().get(5) == Some(&b':'))
    {
        return Err("设置描述不允许网络或设备路径".into());
    }
    Ok(())
}
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
pub fn wasm_metadata(bytes: &[u8]) -> Result<Option<Metadata>, String> {
    if bytes.len() > 32 * 1024 * 1024 || !bytes.starts_with(b"\0asm\x01\0\0\0") {
        return Err("不是受支持的 WASM 模块，或超过 32 MiB".into());
    }
    let mut result = None;
    for (count, payload) in wasmparser::Parser::new(0).parse_all(bytes).enumerate() {
        if count > 65536 {
            return Err("WASM 段数量超限".into());
        }
        if let wasmparser::Payload::CustomSection(section) = payload.map_err(|e| e.to_string())? {
            if section.name() == "weasel.settings" {
                if result.is_some() {
                    return Err("重复的 weasel.settings 段".into());
                }
                result = Some(Metadata::parse(section.data())?);
            }
        }
    }
    Ok(result)
}
fn embedded(defaults: &[u8], rich: &[u8]) -> Result<Metadata, String> {
    Metadata::from_parts(
        serde_json::from_slice(defaults).map_err(|e| e.to_string())?,
        serde_json::from_slice(rich).map_err(|e| e.to_string())?,
    )
}
pub fn load(paths: &RuntimePaths, document: &Document) -> Result<Catalog, String> {
    let mut catalog = Catalog {
        sections: vec![],
        notices: vec![],
    };
    catalog.sections.push(Section {
        title: "常规".into(),
        mount: vec![],
        metadata: embedded(
            include_bytes!("../../weasel.json"),
            include_bytes!("../../weasel.richschema.json"),
        )?,
    });
    let mut budget = 64 * 1024 * 1024;
    for (id, defaults, rich) in [
        (
            "abc",
            include_bytes!("../../themes/abc/src/config.json").as_slice(),
            include_bytes!("../../themes/abc/src/config.richschema.json").as_slice(),
        ),
        (
            "eleven",
            include_bytes!("../../themes/eleven/src/config.json").as_slice(),
            include_bytes!("../../themes/eleven/src/config.richschema.json").as_slice(),
        ),
    ] {
        let path = paths
            .executable_directory
            .join("themes")
            .join(format!("weasel_theme_{id}.settings.json"));
        // 旁置描述损坏时报错而非静默使用不同版本的内置描述。
        let metadata = if path.exists() {
            Metadata::parse(&read(&path, 1024 * 1024, &mut budget)?)?
        } else {
            embedded(defaults, rich)?
        };
        catalog.sections.push(Section {
            title: format!("主题设置 - {id}"),
            mount: vec!["themeSettings".into(), id.into()],
            metadata,
        });
    }
    let effective = document.effective();
    let wasm = &effective["themeSettings"]["wasm"];
    let main = &catalog.sections[0].metadata.richschema["schema"];
    catalog.sections.push(Section { title: "WASM 模块选择与路径".into(), mount: vec!["themeSettings".into(), "wasm".into()],
        metadata: Metadata::from_parts(serde_json::json!({}),
            serde_json::json!({"formatVersion":1,"schema":main["properties"]["themeSettings"]["properties"]["wasm"].clone(),"ui":{"fields":{}}}))? });
    let mut ids = BTreeSet::new();
    if let Some(modules) = wasm["modules"].as_object() {
        ids.extend(modules.keys().cloned());
    }
    if let Some(id) = wasm["theme"].as_str() {
        ids.insert(id.into());
    }
    for dir in [
        &paths.executable_directory.join("theme-wasm"),
        &paths.user_data,
    ] {
        if !dir.exists() {
            continue;
        }
        for (count, entry) in fs::read_dir(dir).map_err(|e| e.to_string())?.enumerate() {
            if count >= 512 {
                return Err("主题扫描目录条目超过 512".into());
            }
            let entry = entry.map_err(|e| e.to_string())?;
            if entry.file_type().is_ok_and(|t| t.is_file())
                && entry.path().extension().is_some_and(|e| e == "wasm")
            {
                if let Some(id) = entry.path().file_stem().and_then(|s| s.to_str()) {
                    ids.insert(id.into());
                }
            }
        }
    }
    if ids.len() > 32 {
        return Err("WASM 主题数量超过 32".into());
    }
    for id in ids {
        if !valid_id(&id) {
            return Err(format!("无效主题标识：{id}"));
        }
        let explicit = wasm["modules"][&id]["file"]
            .as_str()
            .filter(|s| !s.is_empty());
        let candidates = match explicit {
            Some(file) => vec![paths.user_data.join(file)],
            None => vec![
                paths
                    .executable_directory
                    .join("theme-wasm")
                    .join(format!("{id}.wasm")),
                paths.user_data.join(format!("{id}.wasm")),
            ],
        };
        for path in &candidates {
            local_path(path)?;
        }
        let metadata = if let Some(path) = candidates.iter().find(|p| p.exists()) {
            wasm_metadata(&read(path, 32 * 1024 * 1024, &mut budget)?)?
        } else {
            catalog
                .notices
                .push(format!("{id} 模块不存在，使用 JSON 编辑"));
            None
        };
        let metadata = match metadata {
            Some(meta) => meta,
            None => Metadata::from_parts(
                serde_json::json!({}),
                serde_json::json!({"formatVersion":1,"schema":{"type":"object"},"ui":{"fields":{}}}),
            )?,
        };
        catalog.sections.push(Section {
            title: format!("主题设置 - wasm / {id}"),
            mount: vec![
                "themeSettings".into(),
                "wasm".into(),
                "modules".into(),
                id,
                "config".into(),
            ],
            metadata,
        });
    }
    Ok(catalog)
}
