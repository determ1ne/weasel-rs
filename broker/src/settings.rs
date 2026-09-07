//! Broker-owned startup configuration. Files overlay objects recursively;
//! arrays and scalar values replace the previous value.
use serde_json::Value;
use std::{
    io::{self, Read},
    path::Path,
};
use weasel_common::{runtime_paths::RuntimePaths, settings::ConfigSnapshot};

const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

pub fn load(paths: &RuntimePaths, mut warn: impl FnMut(String)) -> ConfigSnapshot {
    // Also keep packaged defaults available to standalone cargo builds.
    let mut value: Value = serde_json::from_str(include_str!("../../weasel.json"))
        .expect("packaged settings must be valid JSON");
    for path in [
        paths.executable_directory.join("weasel.json"),
        paths.user_data.join("weasel.custom.json"),
    ] {
        if let Err(error) = overlay_file(&mut value, &path) {
            warn(format!(
                "settings {}: {error}; keeping previous settings",
                path.display()
            ));
        }
    }
    ConfigSnapshot::new(value)
}

fn overlay_file(base: &mut Value, path: &Path) -> Result<(), String> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    let mut bytes = Vec::new();
    file.take(MAX_CONFIG_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_CONFIG_BYTES {
        return Err("configuration exceeds 1 MiB".into());
    }
    overlay(base, &bytes)
}

fn overlay(base: &mut Value, bytes: &[u8]) -> Result<(), String> {
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    let patch: Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if !patch.is_object() {
        return Err("configuration must be a JSON object".into());
    }
    let mut next = base.clone();
    merge(&mut next, patch);
    // Leave room for protobuf fields when two individually valid files merge.
    if next.to_string().len() > weasel_common::framing::MAX_FRAME_SIZE - 4096 {
        return Err("merged configuration exceeds RPC size limit".into());
    }
    if !matches!(
        next.get("theme").and_then(Value::as_str),
        Some("eleven" | "ten")
    ) {
        return Err("theme must be eleven or ten".into());
    }
    if next.get("inline_preedit").is_some_and(|v| !v.is_boolean()) {
        return Err("inline_preedit must be a boolean".into());
    }
    if next.get("themeSettings").is_some_and(|v| !v.is_object()) {
        return Err("themeSettings must be an object".into());
    }
    *base = next;
    Ok(())
}

fn merge(base: &mut Value, patch: Value) {
    match (base, patch) {
        (Value::Object(base), Value::Object(patch)) => {
            for (key, value) in patch {
                merge(base.entry(key).or_insert(Value::Null), value);
            }
        }
        (base, patch) => *base = patch,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn merged_size_limit_is_atomic() {
        let mut base = json!({"theme":"ten", "first":"x".repeat(600_000)});
        let original = base.clone();
        let patch = serde_json::to_vec(&json!({"second":"y".repeat(600_000)})).unwrap();
        assert!(patch.len() < MAX_CONFIG_BYTES as usize);
        assert!(overlay(&mut base, &patch).unwrap_err().contains("RPC size"));
        assert_eq!(base, original);
    }

    #[test]
    fn merges_objects_and_replaces_arrays() {
        let mut base = json!({"theme":"eleven", "nested":{"a":1,"b":2}, "list":[1,2]});
        overlay(&mut base, br#"{"theme":"ten","nested":{"b":3},"list":[4]}"#).unwrap();
        assert_eq!(
            base,
            json!({"theme":"ten","nested":{"a":1,"b":3},"list":[4]})
        );
    }

    #[test]
    fn invalid_overrides_are_atomic() {
        let original = json!({"theme":"ten"});
        for bytes in [
            b"{".as_slice(),
            b"[]",
            br#"{"theme":null}"#,
            br#"{"theme":"unknown"}"#,
            br#"{"theme":42}"#,
        ] {
            let mut base = original.clone();
            assert!(overlay(&mut base, bytes).is_err());
            assert_eq!(base, original);
        }
    }

    #[test]
    fn empty_override_and_utf8_bom_are_supported() {
        let mut base = json!({"theme":"ten"});
        overlay(&mut base, b"\xef\xbb\xbf{}").unwrap();
        assert_eq!(base["theme"], "ten");
    }

    #[test]
    fn files_follow_precedence_and_missing_files_are_optional() {
        let directory = std::env::temp_dir().join(format!(
            "weasel-settings-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&directory).unwrap();
        let paths = RuntimePaths {
            executable_directory: directory.clone(),
            development: true,
            user_data: directory.join("user-data"),
            logs: directory.join("logs"),
        };
        paths.ensure().unwrap();
        let mut warnings = Vec::new();
        assert_eq!(
            load(&paths, |w| warnings.push(w)).theme().unwrap(),
            "eleven"
        );
        std::fs::write(directory.join("weasel.json"), br#"{"theme":"ten"}"#).unwrap();
        assert_eq!(load(&paths, |w| warnings.push(w)).theme().unwrap(), "ten");
        let custom = paths.user_data.join("weasel.custom.json");
        std::fs::write(&custom, br#"{"theme":"eleven"}"#).unwrap();
        assert_eq!(
            load(&paths, |w| warnings.push(w)).theme().unwrap(),
            "eleven"
        );
        assert!(warnings.is_empty());
        std::fs::write(&custom, b"invalid").unwrap();
        assert_eq!(load(&paths, |w| warnings.push(w)).theme().unwrap(), "ten");
        assert_eq!(warnings.len(), 1);
        std::fs::write(&custom, vec![b' '; MAX_CONFIG_BYTES as usize + 1]).unwrap();
        assert!(overlay_file(&mut serde_json::json!({"theme":"ten"}), &custom).is_err());
        std::fs::remove_dir_all(&directory).unwrap();
    }
}
