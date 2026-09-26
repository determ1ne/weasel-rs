use super::*;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(1);

fn temporary_directory() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "weasel-settings-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
    ))
}

fn apply_patch(base: &mut Value, patch: &Value) -> Result<(), String> {
    let directory = temporary_directory();
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("weasel.custom.json");
    std::fs::write(&path, serde_json::to_vec(patch).unwrap()).unwrap();
    let result = overlay_file(base, &path);
    std::fs::remove_dir_all(directory).unwrap();
    result
}

#[test]
fn merged_size_limit_is_atomic() {
    let mut base = json!({"theme":"ten", "first":"x".repeat(600_000)});
    let original = base.clone();
    let patch = json!({"second":"y".repeat(600_000)});
    assert!(patch.to_string().len() < MAX_CONFIG_BYTES as usize);
    assert!(
        apply_patch(&mut base, &patch)
            .unwrap_err()
            .contains("RPC size")
    );
    assert_eq!(base, original);
}

#[test]
fn merges_objects_and_replaces_arrays() {
    let mut base = json!({"theme":"eleven", "nested":{"a":1,"b":2}, "list":[1,2]});
    apply_patch(
        &mut base,
        &json!({"theme":"ten","nested":{"b":3},"list":[4]}),
    )
    .unwrap();
    assert_eq!(
        base,
        json!({"theme":"ten","nested":{"a":1,"b":3},"list":[4]})
    );
}

#[test]
fn option_values_are_left_for_consumers_to_validate() {
    let mut base = json!({"theme":"ten", "inline_preedit":true});
    apply_patch(
        &mut base,
        &json!({
            "theme": "future-theme",
            "inline_preedit": "invalid",
            "app_options": {"cmd.exe": {"ascii_mode": "invalid"}}
        }),
    )
    .unwrap();
    assert_eq!(base["theme"], "future-theme");
    assert_eq!(base["inline_preedit"], "invalid");
    assert_eq!(base["app_options"]["cmd.exe"]["ascii_mode"], "invalid");
}

#[test]
fn empty_override_is_supported() {
    let mut base = json!({"theme":"ten"});
    apply_patch(&mut base, &json!({})).unwrap();
    assert_eq!(base["theme"], "ten");
}

#[test]
fn files_follow_precedence_and_missing_files_are_optional() {
    let directory = temporary_directory();
    std::fs::create_dir(&directory).unwrap();
    let paths = RuntimePaths {
        executable_directory: directory.clone(),
        user_data: directory.join("user-data"),
        logs: directory.join("logs"),
    };
    paths.ensure().unwrap();
    let mut warnings = Vec::new();
    assert_eq!(
        load(&paths, |w| warnings.push(w))
            .required::<String>(".theme")
            .unwrap(),
        "eleven"
    );
    std::fs::write(directory.join("weasel.json"), br#"{"theme":"ten"}"#).unwrap();
    assert_eq!(
        load(&paths, |w| warnings.push(w))
            .required::<String>(".theme")
            .unwrap(),
        "ten"
    );
    let custom = paths.user_data.join("weasel.custom.json");
    std::fs::write(&custom, b"\xef\xbb\xbf{\"theme\":\"eleven\"}").unwrap();
    assert_eq!(
        load(&paths, |w| warnings.push(w))
            .required::<String>(".theme")
            .unwrap(),
        "eleven"
    );
    assert!(warnings.is_empty());
    std::fs::write(&custom, b"invalid").unwrap();
    assert_eq!(
        load(&paths, |w| warnings.push(w))
            .required::<String>(".theme")
            .unwrap(),
        "ten"
    );
    assert_eq!(warnings.len(), 1);
    std::fs::write(&custom, vec![b' '; MAX_CONFIG_BYTES as usize + 1]).unwrap();
    assert!(overlay_file(&mut json!({"theme":"ten"}), &custom).is_err());
    std::fs::remove_dir_all(&directory).unwrap();
}
