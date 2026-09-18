#[path = "../build_support/icon.rs"]
mod icon;
fn main() {
    icon::embed();
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("renderer.manifest");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", manifest.display());
    println!("cargo:rustc-link-arg-bin=weasel-renderer=/MANIFEST:EMBED");
    // renderer.manifest owns requestedExecutionLevel, including uiAccess.
    // Disable the linker's default fragment so future changes cannot conflict.
    println!("cargo:rustc-link-arg-bin=weasel-renderer=/MANIFESTUAC:NO");
    println!(
        "cargo:rustc-link-arg-bin=weasel-renderer=/MANIFESTINPUT:{}",
        manifest.display()
    );
}
