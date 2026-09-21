//! Rust WASM主题在build.rs中调用，直接cargo build也携带声明式配置。
//! 调用方添加serde_json构建依赖，并include生成的theme_metadata.rs。
pub fn generate() {
    use std::{env, fs, path::PathBuf};
    println!("cargo:rerun-if-changed=config.json");
    println!("cargo:rerun-if-changed=config.richschema.json");
    let read = |path| -> serde_json::Value {
        serde_json::from_slice(&fs::read(path).expect("read theme metadata"))
            .expect("theme metadata JSON")
    };
    let defaults = read("config.json");
    let richschema = read("config.richschema.json");
    assert!(defaults.is_object());
    assert_eq!(richschema["formatVersion"], 1);
    assert_eq!(richschema["schema"]["type"], "object");
    let bytes = serde_json::to_vec(
        &serde_json::json!({"formatVersion": 1, "defaults": defaults, "richschema": richschema}),
    )
    .unwrap();
    assert!(bytes.len() <= 1024 * 1024, "theme metadata exceeds 1 MiB");
    let code = format!(
        "// Generated; do not edit.\n#[cfg(target_arch = \"wasm32\")]\n#[used]\n#[unsafe(link_section = \"weasel.settings\")]\nstatic THEME_METADATA: [u8; {}] = {:?};\n",
        bytes.len(),
        bytes
    );
    fs::write(
        PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("theme_metadata.rs"),
        code,
    )
    .unwrap();
}
