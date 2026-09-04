use std::path::PathBuf;
mod build_ui;
#[path = "../build_support/icon.rs"]
mod icon;

fn main() {
    icon::embed();
    build_ui::generate();
    println!("cargo:rerun-if-changed=build_ui.rs");
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let header = manifest_dir.join("../vendor/librime/include/rime_api.h");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR is not set"));
    let output = out.join("rime_bindings.rs");

    windows_bindgen::builder()
        .input_default()
        .output(out.join("bindings.rs"))
        .filters([
            "Windows.Win32.CREATE_NO_WINDOW",
            "Windows.Win32.AllocConsole",
            "Windows.Win32.LoadLibraryExW",
            "Windows.Win32.LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR",
            "Windows.Win32.LOAD_LIBRARY_SEARCH_SYSTEM32",
            "Windows.Win32.GetProcAddress",
            "Windows.Win32.FreeLibrary",
        ])
        .flat()
        .write();

    println!("cargo:rerun-if-changed=build.rs");

    println!("cargo:rerun-if-changed={}", header.display());

    bindgen::Builder::default()
        .header(header.to_string_lossy())
        .allowlist_type("Bool")
        .allowlist_type("Rime.*")
        .layout_tests(false)
        .generate()
        .expect("failed to generate librime bindings")
        .write_to_file(output)
        .expect("failed to write librime bindings");
}
