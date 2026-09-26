//! 发现并组装设置界面使用的内置、原生主题及 WASM 主题配置描述。
//!
//! 本模块只读取 JSON 描述和 WASM 自定义元数据段，不加载主题 DLL，也不实例化
//! WASM。目录扫描、文件大小和累计读取量均有限制；路径只允许本机文件系统路径。
use crate::{config::Document, metadata::Metadata};
use std::{collections::BTreeSet, fs, io::Read, path::Path};
use weasel_common::process::RuntimePaths;

/// 可独立编辑并挂载到完整设置树某一位置的配置分区。
pub struct Section {
    /// 界面显示的分区标题。
    pub title: String,
    /// 配置值在完整配置树中的挂载路径；空路径表示根配置。
    pub mount: Vec<String>,
    /// 本分区对应的默认值与 Schema 元数据。
    pub metadata: Metadata,
}
/// 设置界面的分区集合及加载过程中产生的提示。
pub struct Catalog {
    /// 按界面顺序排列的配置分区。
    pub sections: Vec<Section>,
    /// 可继续以 JSON 方式编辑的提示，例如找不到对应 WASM 模块。
    pub notices: Vec<String>,
}

/// 在单文件上限及共享读取预算内读取普通本地文件。
///
/// 读取前后都会检查路径，规范化路径以解析符号链接；文件最多读取 `limit + 1`
/// 字节以识别超限。成功读取才扣减共享预算，路径、文件类型、I/O 或预算错误均
/// 返回 `Err`。
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
/// 拒绝 UNC 网络共享和设备路径，同时允许盘符形式的 Windows 扩展路径。
fn local_path(path: &Path) -> Result<(), String> {
    let text = path.to_string_lossy().replace('/', "\\");
    if text.starts_with("\\\\")
        && !(text.starts_with("\\\\?\\") && text.as_bytes().get(5) == Some(&b':'))
    {
        return Err("设置描述不允许网络或设备路径".into());
    }
    Ok(())
}
/// 判断主题标识是否非空、长度不超过 128 字节，且仅含 ASCII 字母、数字、`-`、`_`。
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
/// 从 WASM 字节中读取元数据段；没有该段时返回 `Ok(None)`。
///
/// 找到元数据时会提取 `defaults` 与 `richschema` 并完整验证；WASM 段解析或
/// 元数据校验失败均返回 `Err`。本函数只读字节，不加载或执行模块。
pub fn wasm_metadata(bytes: &[u8]) -> Result<Option<Metadata>, String> {
    weasel_common::wasm_metadata::read(bytes)?
        .map(|mut m| Metadata::from_parts(m["defaults"].take(), m["richschema"].take()))
        .transpose()
}
/// 解析仓库/程序内嵌的默认值和扩展 Schema，并执行与外部元数据相同的验证。
fn embedded(defaults: &[u8], rich: &[u8]) -> Result<Metadata, String> {
    Metadata::from_parts(
        serde_json::from_slice(defaults).map_err(|e| e.to_string())?,
        serde_json::from_slice(rich).map_err(|e| e.to_string())?,
    )
}
/// 根据运行时目录和当前有效配置构造设置目录。
///
/// 固定分区使用内嵌描述；存在旁置原生主题描述时优先读取，损坏则报错而不回退。
/// WASM 标识来自配置及两个候选目录中的 `.wasm` 文件名，最多扫描每个目录 512
/// 项、接受最多 32 个标识。文件读取共享 64 MiB 预算，单个 WASM 文件上限为
/// 32 MiB。缺少模块时保留通用对象 Schema 并附加提示；其他读取或验证错误中止加载。
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
