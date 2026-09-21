//! 设置应用与运行时共用的离线WASM元数据读取；不执行模块，不解析指针。
use serde_json::Value;

pub const SECTION: &str = "weasel.settings";
pub const MAX_BYTES: usize = 1024 * 1024;

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
