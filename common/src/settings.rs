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

    pub fn app_ascii_mode(&self, executable: &str) -> Option<bool> {
        self.app_bool(executable, "ascii_mode")
    }

    /// 应用未指定时继承全局值，不能在应用层填默认值而固定继承结果。
    pub fn app_bool(&self, executable: &str, option: &str) -> Option<bool> {
        self.application_bool(executable, option)
            .or_else(|| self.0.get(option).and_then(Value::as_bool))
    }

    pub fn inline_preedit(&self) -> bool {
        self.0
            .get("inline_preedit")
            .and_then(Value::as_bool)
            .unwrap_or(true)
    }

    pub fn app_inline_preedit(&self, executable: &str) -> bool {
        self.app_bool(executable, "inline_preedit")
            .unwrap_or_else(|| self.inline_preedit())
    }

    /// Negotiate renderer capabilities early if any application can need them.
    pub fn needs_external_preedit(&self) -> bool {
        !self.inline_preedit()
            || self
                .0
                .get("app_options")
                .and_then(Value::as_object)
                .is_some_and(|apps| {
                    apps.values().any(|options| {
                        options.get("inline_preedit").and_then(Value::as_bool) == Some(false)
                    })
                })
    }

    fn application_bool(&self, executable: &str, option: &str) -> Option<bool> {
        let apps = self.0.get("app_options")?.as_object()?;
        apps.get(executable)
            .or_else(|| {
                apps.iter()
                    .find(|(name, _)| name.eq_ignore_ascii_case(executable))
                    .map(|(_, value)| value)
            })?
            .get(option)?
            .as_bool()
    }
    pub fn theme_settings<T: DeserializeOwned + Default>(&self, name: &str) -> Result<T, String> {
        let key = serde_json::to_string(name).map_err(|e| e.to_string())?;
        Ok(self
            .get(&format!(".themeSettings[{key}]"))?
            .unwrap_or_default())
    }

    /// Add one factory's defaults without changing the broker snapshot or other
    /// themes. The already-merged installation/user object has final precedence.
    pub fn with_theme_defaults(&self, name: &str, mut defaults: Value) -> Result<Self, String> {
        if !defaults.is_object() {
            return Err(format!("theme {name} defaults must be an object"));
        }
        let mut root = (*self.0).clone();
        let root_object = root
            .as_object_mut()
            .ok_or("configuration root must be an object")?;
        let themes = root_object
            .entry("themeSettings")
            .or_insert_with(|| Value::Object(Default::default()))
            .as_object_mut()
            .ok_or("themeSettings must be an object")?;
        if let Some(overrides) = themes.get(name) {
            if !overrides.is_object() {
                return Err(format!("themeSettings.{name} must be an object"));
            }
            merge(&mut defaults, overrides.clone());
        }
        themes.insert(name.into(), defaults);
        Ok(Self::new(root))
    }
}

/// Objects merge recursively; arrays, scalars and explicit null replace values.
pub fn merge(base: &mut Value, patch: Value) {
    match (base, patch) {
        (Value::Object(base), Value::Object(patch)) => {
            for (key, value) in patch {
                merge(base.entry(key).or_insert(Value::Null), value);
            }
        }
        (base, patch) => *base = patch,
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
    #[test]
    fn application_preedit_overrides_global_and_enables_early_negotiation() {
        for global in [true, false] {
            let config = super::ConfigSnapshot::new(serde_json::json!({
                "inline_preedit": global,
                "app_options": {
                    "cmd.exe": {"inline_preedit": false},
                    "editor.exe": {"inline_preedit": true},
                    "other.exe": {"ascii_mode": true}
                }
            }));
            assert!(!config.app_inline_preedit("CMD.EXE"));
            assert!(config.app_inline_preedit("editor.exe"));
            assert_eq!(config.app_inline_preedit("other.exe"), global);
            assert_eq!(config.app_inline_preedit("unknown.exe"), global);
            assert!(config.needs_external_preedit());
        }
        let defaults = super::ConfigSnapshot::new(serde_json::json!({}));
        assert!(defaults.app_inline_preedit("cmd.exe"));
        assert!(!defaults.needs_external_preedit());
    }
    #[test]
    fn application_defaults_distinguish_false_from_missing() {
        let config = super::ConfigSnapshot::new(serde_json::json!({
            "app_options": {"cmd.exe": {"ascii_mode": true}, "editor.exe": {"ascii_mode": false}}
        }));
        assert_eq!(config.app_ascii_mode("CMD.EXE"), Some(true));
        assert_eq!(config.app_ascii_mode("editor.exe"), Some(false));
        assert_eq!(config.app_ascii_mode("unknown.exe"), None);
        let config = super::ConfigSnapshot::new(serde_json::json!({
            "ascii_mode": true, "inline_preedit": false,
            "app_options": {"editor.exe": {"ascii_mode": false}}
        }));
        assert_eq!(config.app_ascii_mode("unknown.exe"), Some(true));
        assert_eq!(config.app_ascii_mode("EDITOR.EXE"), Some(false));
        assert!(!config.inline_preedit());
        for global in [false, true] {
            let config = super::ConfigSnapshot::new(serde_json::json!({
                "ascii_mode": global, "inline_preedit": global,
                "app_options": {"empty.exe": {}, "explicit.exe": {
                    "ascii_mode": false, "inline_preedit": false
                }}
            }));
            assert_eq!(config.app_ascii_mode("EMPTY.EXE"), Some(global));
            assert_eq!(config.app_inline_preedit("EMPTY.EXE"), global);
            assert_eq!(config.app_ascii_mode("explicit.exe"), Some(false));
            assert!(!config.app_inline_preedit("explicit.exe"));
        }
    }
    use super::*;
    #[test]
    fn theme_defaults_are_overlaid_without_changing_the_source() {
        let source = ConfigSnapshot::new(serde_json::json!({
            "inline_preedit": true,
            "themeSettings": {"abc": {"antialiasing": false, "nested": {"b": 3}}}
        }));
        let merged = source
            .with_theme_defaults(
                "abc",
                serde_json::json!({
                    "antialiasing": true, "nested": {"a": 1, "b": 2}
                }),
            )
            .unwrap();
        assert_eq!(
            merged
                .get::<bool>(".themeSettings.abc.antialiasing")
                .unwrap(),
            Some(false)
        );
        assert_eq!(
            merged.get::<i32>(".themeSettings.abc.nested.a").unwrap(),
            Some(1)
        );
        assert_eq!(
            merged.get::<i32>(".themeSettings.abc.nested.b").unwrap(),
            Some(3)
        );
        assert!(
            source
                .query(".themeSettings.abc.nested.a")
                .unwrap()
                .is_none()
        );
        assert_eq!(merged.get::<bool>(".inline_preedit").unwrap(), Some(true));
        let fallback = source
            .with_theme_defaults("ten", serde_json::json!({"size": 12}))
            .unwrap();
        assert_eq!(
            fallback.get::<i32>(".themeSettings.ten.size").unwrap(),
            Some(12)
        );
    }
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
