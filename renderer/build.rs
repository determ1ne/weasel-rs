#[path = "../build_support/icon.rs"]
mod icon;
fn main() {
    icon::embed();
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("renderer.manifest");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", manifest.display());
    println!("cargo:rerun-if-env-changed=WEASEL_RENDERER_UIACCESS");
    let uiaccess = std::env::var("WEASEL_RENDERER_UIACCESS").unwrap_or_default();
    assert!(
        matches!(uiaccess.as_str(), "" | "0" | "1"),
        "invalid WEASEL_RENDERER_UIACCESS"
    );
    let manifest = if uiaccess == "1" {
        let source = std::fs::read_to_string(&manifest).expect("read renderer manifest");
        assert!(source.contains("uiAccess=\"false\""));
        let output = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap())
            .join("renderer-uiaccess.manifest");
        std::fs::write(
            &output,
            source.replace("uiAccess=\"false\"", "uiAccess=\"true\""),
        )
        .expect("write UIAccess manifest");
        output
    } else {
        manifest
    };
    println!("cargo:rustc-link-arg-bin=weasel-renderer=/MANIFEST:EMBED");
    // renderer.manifest owns requestedExecutionLevel, including uiAccess.
    // Disable the linker's default fragment so future changes cannot conflict.
    println!("cargo:rustc-link-arg-bin=weasel-renderer=/MANIFESTUAC:NO");
    println!(
        "cargo:rustc-link-arg-bin=weasel-renderer=/MANIFESTINPUT:{}",
        manifest.display()
    );
}
