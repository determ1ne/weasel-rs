//! Shared executable icon resource; manifests remain owned by each binary.
#[path = "version.rs"]
mod version;

pub fn embed() {
    version::embed();
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let resource = root.join("assets/weasel.rc");
    let icon = root.join("assets/weasel.ico");
    println!("cargo:rerun-if-changed={}", resource.display());
    println!("cargo:rerun-if-changed={}", icon.display());
    println!(
        "cargo:rerun-if-changed={}",
        root.join("build_support/icon.rs").display()
    );
    let icon = icon
        .to_str()
        .expect("icon path must be UTF-8")
        .replace('\\', "/");
    let definition = format!("WEASEL_ICON_PATH=\"{icon}\"");
    embed_resource::compile_for(resource, [env!("CARGO_PKG_NAME")], &[definition])
        .manifest_required()
        .expect("failed to embed executable icon");
}
