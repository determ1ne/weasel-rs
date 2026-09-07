fn main() {
    build_identity();
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR is set"));
    windows_bindgen::builder()
        .output(out.join("bindings.rs"))
        .input_default()
        .filters([
            "Windows.Win32.GetKeyState",
            "Windows.Win32.VK_SHIFT",
            "Windows.Win32.MessageBoxW",
            "Windows.Win32.MB_OK",
            "Windows.Win32.MB_SETFOREGROUND",
            "Windows.Win32.OutputDebugStringW",
            "Windows.Win32.GetCurrentProcess",
            "Windows.Win32.OpenProcessToken",
            "Windows.Win32.GetTokenInformation",
            "Windows.Win32.ConvertSidToStringSidW",
            "Windows.Win32.ConvertStringSecurityDescriptorToSecurityDescriptorW",
            "Windows.Win32.ConvertSecurityDescriptorToStringSecurityDescriptorW",
            "Windows.Win32.GetSecurityInfo",
            "Windows.Win32.CloseHandle",
            "Windows.Win32.LocalFree",
            "Windows.Win32.CreateMutexW",
            "Windows.Win32.GetLastError",
            "Windows.Win32.TOKEN_USER",
            "Windows.Win32.TOKEN_GROUPS",
            "Windows.Win32.TOKEN_QUERY",
            "Windows.Win32.TokenUser",
            "Windows.Win32.TokenLogonSid",
            "Windows.Win32.SE_GROUP_LOGON_ID",
            "Windows.Win32.SDDL_REVISION_1",
            "Windows.Win32.SECURITY_ATTRIBUTES",
            "Windows.Win32.ERROR_ALREADY_EXISTS",
            "Windows.Win32.ERROR_INVALID_PARAMETER",
            "Windows.Win32.DACL_SECURITY_INFORMATION",
            "Windows.Win32.SACL_SECURITY_INFORMATION",
            "Windows.Win32.LABEL_SECURITY_INFORMATION",
            "Windows.Win32.SE_KERNEL_OBJECT",
        ])
        .flat()
        .write();
    println!("cargo:rerun-if-changed=build.rs");
    let protoc = protoc_bin_vendored::protoc_bin_path().expect("protoc is available");
    unsafe { std::env::set_var("PROTOC", protoc) };

    println!("cargo:rerun-if-changed=proto/message.proto");
    println!("cargo:rerun-if-changed=proto/rpc.proto");
    prost_build::compile_protos(&["proto/message.proto", "proto/rpc.proto"], &["proto"])
        .expect("message.proto compiles");
}

fn build_identity() {
    use std::{
        process::Command,
        time::{SystemTime, UNIX_EPOCH},
    };
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let git = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .current_dir(&root)
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    };
    let revision = git(&["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let dirty = git(&["status", "--porcelain", "--untracked-files=normal"])
        .map(|status| if status.is_empty() { "clean" } else { "dirty" })
        .unwrap_or("unknown");
    let epoch = std::env::var("SOURCE_DATE_EPOCH").unwrap_or_else(|_| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .to_string()
    });
    println!("cargo:rustc-env=WEASEL_BUILD_REVISION={revision}");
    println!("cargo:rustc-env=WEASEL_BUILD_STATE={dirty}");
    println!("cargo:rustc-env=WEASEL_BUILD_EPOCH={epoch}");
    let date = epoch.parse::<i64>().ok().and_then(|seconds| {
        Command::new("powershell.exe").args(["-NoProfile", "-NonInteractive", "-Command",
            &format!("[DateTimeOffset]::FromUnixTimeSeconds({seconds}).UtcDateTime.ToString('yyyy-MM-dd HH:mm:ss')")])
            .output().ok().filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }).unwrap_or_else(|| format!("Unix seconds {epoch}"));
    println!("cargo:rustc-env=WEASEL_BUILD_DATE={date}");
    println!(
        "cargo:rustc-env=WEASEL_BUILD_TARGET={}",
        std::env::var("TARGET").unwrap()
    );
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    for path in [
        ".git/HEAD",
        ".git/index",
        ".git/refs",
        ".git/packed-refs",
        "tip",
        "broker",
        "common",
        "renderer",
        "server",
        "build_support",
        "Cargo.lock",
    ] {
        println!("cargo:rerun-if-changed={}", root.join(path).display());
    }
}
