fn main() {
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR is set"));
    windows_bindgen::builder()
        .output(out.join("bindings.rs"))
        .input_default()
        .filters([
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
