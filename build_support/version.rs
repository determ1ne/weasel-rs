//! Native file properties shared by EXEs and the TIP DLL.
pub fn embed() {
    let package = std::env::var("CARGO_PKG_NAME").expect("Cargo package name");
    let (description, filename, dll) = match package.as_str() {
        "weasel-broker" => ("Weasel-RS Broker", "weasel-broker.exe", false),
        "weasel-server" => ("Weasel-RS Rime Server", "weasel-server.exe", false),
        "weasel-renderer" => ("Weasel-RS Candidate Renderer", "weasel-renderer.exe", false),
        "weasel-tip" => ("Weasel-RS Text Input Processor", "weasel_tip.dll", true),
        "weasel-theme-ten" => ("Weasel-RS ten Theme", "weasel_theme_ten.dll", true),
        "weasel-theme-eleven" => ("Weasel-RS eleven Theme", "weasel_theme_eleven.dll", true),
        "weasel-theme-abc" => ("Weasel-RS abc Theme", "weasel_theme_abc.dll", true),
        "weasel-theme-void" => ("Weasel-RS void Theme", "weasel_theme_void.dll", true),
        "weasel-theme-wasm" => ("Weasel-RS wasm Theme", "weasel_theme_wasm.dll", true),
        _ => panic!("missing Windows file description for {package}"),
    };
    let root = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("..");
    let root = if package.starts_with("weasel-theme-") {
        root.join("..")
    } else {
        root
    };
    let resource = root.join("build_support/version.rc");
    for path in [&resource, &root.join("build_support/version.rs")] {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    println!("cargo:rerun-if-changed=Cargo.toml");
    let version = std::env::var("CARGO_PKG_VERSION").expect("Cargo package version");
    let numeric: Vec<_> = ["MAJOR", "MINOR", "PATCH"]
        .map(|part| {
            std::env::var(format!("CARGO_PKG_VERSION_{part}"))
                .expect("Cargo version component")
                .parse::<u16>()
                .expect("Windows version component must fit in 16 bits")
        })
        .into_iter()
        .collect();
    let quote = |name: &str, value: &str| {
        format!(
            "{name}=\"{}\"",
            value.replace('\\', "\\\\").replace('"', "\\\"")
        )
    };
    let definitions = [
        format!(
            "WEASEL_FILE_VERSION={},{},{},0",
            numeric[0], numeric[1], numeric[2]
        ),
        format!("WEASEL_FILE_TYPE={}", if dll { 2 } else { 1 }),
        quote("WEASEL_FILE_DESCRIPTION", description),
        quote("WEASEL_VERSION_STRING", &version),
        quote("WEASEL_INTERNAL_NAME", &package),
        quote("WEASEL_ORIGINAL_FILENAME", filename),
    ];
    let result = if dll {
        embed_resource::compile_for_cdylib(resource, &definitions)
    } else {
        embed_resource::compile_for(resource, [package.as_str()], &definitions)
    };
    result
        .manifest_required()
        .expect("failed to embed Windows VERSIONINFO");
}
