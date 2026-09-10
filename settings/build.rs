#[path = "../build_support/icon.rs"]
mod icon;

fn main() {
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    windows_bindgen::builder()
        .input_default()
        .output(out.join("bindings.rs"))
        .filters([
            "Windows.Win32.DwmSetWindowAttribute",
            "Windows.Win32.FindWindowW",
            "Windows.Win32.PostMessageW",
            "Windows.Win32.WM_COMMAND",
            "Windows.Win32.DwmExtendFrameIntoClientArea",
            "Windows.Win32.DWMWA_SYSTEMBACKDROP_TYPE",
            "Windows.Win32.DWMSBT_MAINWINDOW",
            "Windows.Win32.MARGINS",
            "Windows.Win32.GetWindowLongW",
            "Windows.Win32.GWL_STYLE",
            "Windows.Win32.DwmGetWindowAttribute",
            "Windows.Win32.SetWindowSubclass",
            "Windows.Win32.RemoveWindowSubclass",
            "Windows.Win32.DefSubclassProc",
            "Windows.Win32.DefWindowProcW",
            "Windows.Win32.DwmDefWindowProc",
            "Windows.Win32.SetWindowPos",
            "Windows.Win32.GetWindowRect",
            "Windows.Win32.GetDpiForWindow",
            "Windows.Win32.GetSystemMetricsForDpi",
            "Windows.Win32.IsZoomed",
            "Windows.Win32.NCCALCSIZE_PARAMS",
            "Windows.Win32.WM_NCCALCSIZE",
            "Windows.Win32.WM_NCHITTEST",
            "Windows.Win32.WM_NCMOUSEMOVE",
            "Windows.Win32.WM_NCMOUSELEAVE",
            "Windows.Win32.TrackMouseEvent",
            "Windows.Win32.TRACKMOUSEEVENT",
            "Windows.Win32.TME_NONCLIENT",
            "Windows.Win32.TME_LEAVE",
            "Windows.Win32.WM_NCDESTROY",
            "Windows.Win32.HTCLIENT",
            "Windows.Win32.HTCAPTION",
            "Windows.Win32.HTCLOSE",
            "Windows.Win32.HTMINBUTTON",
            "Windows.Win32.HTMAXBUTTON",
            "Windows.Win32.DWMWA_CAPTION_BUTTON_BOUNDS",
            "Windows.Win32.MonitorFromWindow",
            "Windows.Win32.GetMonitorInfoW",
            "Windows.Win32.MONITORINFO",
            "Windows.Win32.MONITOR_DEFAULTTONEAREST",
            "Windows.Win32.WS_MAXIMIZEBOX",
            "Windows.Win32.WS_MINIMIZEBOX",
            "Windows.Win32.HTTOP",
            "Windows.Win32.HTTOPLEFT",
            "Windows.Win32.HTTOPRIGHT",
            "Windows.Win32.SM_CYSIZEFRAME",
            "Windows.Win32.SM_CXPADDEDBORDER",
            "Windows.Win32.SWP_FRAMECHANGED",
            "Windows.Win32.SWP_NOMOVE",
            "Windows.Win32.SWP_NOSIZE",
            "Windows.Win32.SWP_NOZORDER",
            "Windows.Win32.SWP_NOACTIVATE",
        ])
        .flat()
        .write();
    slint_build::compile("ui/main.slint").expect("settings UI must compile");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        icon::embed();
        let manifest =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("settings.manifest");
        println!("cargo:rerun-if-changed={}", manifest.display());
        println!("cargo:rustc-link-arg-bin=weasel-settings=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg-bin=weasel-settings=/MANIFESTINPUT:{}",
            manifest.display()
        );
        // Slint 在 Windows 的调试构建需要较大的主线程栈，仅作用于设置程序。
        println!("cargo:rustc-link-arg-bin=weasel-settings=/STACK:8000000");
    }
}
