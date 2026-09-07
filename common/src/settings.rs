//! Read-only configuration queries. Transport is asynchronous; snapshots are local.
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct ConfigSnapshot(Arc<Value>);

impl ConfigSnapshot {
    pub fn new(value: Value) -> Self {
        Self(Arc::new(value))
    }
    pub fn query(&self, path: &str) -> Result<Option<&Value>, String> {
        query(&self.0, path)
    }
    pub fn get<T: DeserializeOwned>(&self, path: &str) -> Result<Option<T>, String> {
        self.query(path)?
            .map(|v| serde_json::from_value(v.clone()).map_err(|e| e.to_string()))
            .transpose()
    }
    pub fn theme(&self) -> Result<String, String> {
        Ok(self.get(".theme")?.unwrap_or_else(|| "eleven".into()))
    }
    pub fn theme_settings<T: DeserializeOwned + Default>(&self, name: &str) -> Result<T, String> {
        let key = serde_json::to_string(name).map_err(|e| e.to_string())?;
        Ok(self
            .get(&format!(".themeSettings[{key}]"))?
            .unwrap_or_default())
    }
}

// Deliberately only jq-like paths, not an expression language. Validate the
// entire path even after a missing key, so malformed queries are never accepted.
pub fn query<'a>(root: &'a Value, path: &str) -> Result<Option<&'a Value>, String> {
    if path.len() > 4096 {
        return Err("configuration path exceeds 4096 bytes".into());
    }
    if path == "." {
        return Ok(Some(root));
    }
    let mut rest = path;
    let mut current = Some(root);
    while !rest.is_empty() {
        if let Some(tail) = rest.strip_prefix('.') {
            let end = tail.find(['.', '[']).unwrap_or(tail.len());
            let key = &tail[..end];
            if key.is_empty() || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                return Err("expected a property name after '.'".into());
            }
            current = current.and_then(|v| v.as_object()?.get(key));
            rest = &tail[end..];
        } else if let Some(tail) = rest.strip_prefix('[') {
            if tail.starts_with('"') {
                let mut stream = serde_json::Deserializer::from_str(tail).into_iter::<String>();
                let key = stream
                    .next()
                    .ok_or("missing quoted property")?
                    .map_err(|e| e.to_string())?;
                rest = tail[stream.byte_offset()..]
                    .strip_prefix(']')
                    .ok_or("expected ']' after property")?;
                current = current.and_then(|v| v.as_object()?.get(&key));
            } else {
                let end = tail.find(']').ok_or("expected ']' after index")?;
                let text = &tail[..end];
                if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
                    return Err("invalid array index".into());
                }
                let index: usize = text.parse().map_err(|_| "array index too large")?;
                current = current.and_then(|v| v.as_array()?.get(index));
                rest = &tail[end + 1..];
            }
        } else {
            return Err("expected '.' or '[' in configuration path".into());
        }
    }
    if path.is_empty() {
        return Err("empty configuration path; use '.' for root".into());
    }
    Ok(current)
}

pub async fn fetch(
    role: crate::message::PeerRole,
    refresh: bool,
) -> Result<ConfigSnapshot, String> {
    use crate::rpc::{RpcClient, try_default_broker_pipe_name};
    use std::time::Duration;
    tokio::time::timeout(Duration::from_secs(2), async {
        let pipe = try_default_broker_pipe_name().map_err(|e| e.to_string())?;
        let client = RpcClient::connect_as_with_timeout(pipe, role, Duration::from_secs(2))
            .await
            .map_err(|e| e.to_string())?;
        let root = client
            .query_config(".", refresh)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("broker returned no configuration root")?;
        if !root.is_object() {
            return Err("configuration root must be an object".into());
        }
        Ok(ConfigSnapshot::new(root))
    })
    .await
    .map_err(|_| "configuration query timed out".to_owned())?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paths_preserve_objects_null_and_missing() {
        let root = serde_json::json!({"a.b": [null, {"color":"red"}]});
        assert_eq!(
            query(&root, "[\"a.b\"][1].color").unwrap(),
            Some(&serde_json::json!("red"))
        );
        assert_eq!(query(&root, "[\"a.b\"][0]").unwrap(), Some(&Value::Null));
        assert!(query(&root, ".missing").unwrap().is_none());
        assert!(query(&root, ".missing | anything").is_err());
        assert_eq!(query(&root, ".").unwrap(), Some(&root));
    }
}
