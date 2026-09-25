//! 读取 WASM 主题随模块携带的设置元数据。
//!
//! 本模块只解析 WASM 自定义段中的 JSON 描述。
use serde_json::Value;

/// 存放 Weasel 设置描述的 WASM 自定义段名称。
pub const SECTION: &str = "weasel.settings";
/// 自定义段中元数据 JSON 的最大字节数。
pub const MAX_BYTES: usize = 1024 * 1024;

/// 从 WASM 二进制中读取并验证 Weasel 设置元数据。
///
/// 模块不含目标自定义段时返回 `Ok(None)`；存在时返回已解析的 JSON 对象。只接受当前
/// 支持的 WASM 二进制格式和元数据封套版本，并拒绝重复段、未知顶层字段或超过资源
/// 限制的输入。
///
/// # Errors
///
/// 输入不是受支持的 WASM 模块、模块或元数据超限、段重复、JSON 无效、结构过深/过大，
/// 或元数据封套不符合约定时返回错误。
pub fn read(bytes: &[u8]) -> Result<Option<Value>, String> {
    if bytes.len() > 32 * 1024 * 1024 || !bytes.starts_with(b"\0asm\x01\0\0\0") {
        return Err("unsupported WASM module or exceeds 32 MiB".into());
    }
    let mut metadata = None;
    for (count, payload) in wasmparser::Parser::new(0).parse_all(bytes).enumerate() {
        if count > 65536 {
            return Err("too many WASM sections".into());
        }
        if let wasmparser::Payload::CustomSection(section) = payload.map_err(|e| e.to_string())? {
            if section.name() != SECTION {
                continue;
            }
            if metadata.is_some() {
                return Err("duplicate weasel.settings section".into());
            }
            if section.data().len() > MAX_BYTES {
                return Err("theme metadata exceeds 1 MiB".into());
            }
            let value: Value = serde_json::from_slice(section.data())
                .map_err(|e| format!("theme metadata JSON: {e}"))?;
            bounded(&value, 0, &mut 0)?;
            let obj = value
                .as_object()
                .ok_or("theme metadata must be an object")?;
            if obj.get("formatVersion").and_then(Value::as_u64) != Some(1)
                || obj
                    .keys()
                    .any(|k| !matches!(k.as_str(), "formatVersion" | "defaults" | "richschema"))
                || !value["defaults"].is_object()
                || value["richschema"]["formatVersion"] != 1
                || value["richschema"]["schema"]["type"] != "object"
            {
                return Err("invalid theme metadata envelope/defaults/richschema".into());
            }
            metadata = Some(value);
        }
    }
    Ok(metadata)
}

/// 限制 JSON 树的嵌套深度和节点总数，避免处理异常复杂的元数据。
fn bounded(value: &Value, depth: usize, nodes: &mut usize) -> Result<(), String> {
    *nodes += 1;
    if depth > 32 || *nodes > 8192 {
        return Err("theme metadata structure exceeds limits".into());
    }
    match value {
        Value::Array(values) => {
            for v in values {
                bounded(v, depth + 1, nodes)?;
            }
        }
        Value::Object(values) => {
            for v in values.values() {
                bounded(v, depth + 1, nodes)?;
            }
        }
        _ => {}
    }
    Ok(())
}
