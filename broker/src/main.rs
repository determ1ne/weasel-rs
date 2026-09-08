//! The per-user broker: owns the tray icon and the server/renderer children.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod lifecycle;
mod managed_children;
mod notifications;
mod settings;
mod settings_rpc;
#[cfg(windows)]
mod shortcut;
mod toast;

#[cfg(windows)]
mod bindings;

#[cfg(windows)]
mod menu_actions;
#[cfg(windows)]
mod windows_tray;

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Installer commands must not initialize the tray, console or managed children.
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() == Some(std::ffi::OsStr::new("--install-shortcut")) {
        let path = args.next().ok_or("missing shortcut path")?;
        if args.next().is_some() {
            return Err("unexpected shortcut arguments".into());
        }
        return shortcut::install(std::path::Path::new(&path));
    }
    if weasel_common::runtime_paths::is_development() {
        unsafe {
            let _ = bindings::AllocConsole();
        }
    }
    if let Err(error) = windows_tray::run() {
        eprintln!("weasel-broker: {error}");
        unsafe {
            let _ = bindings::MessageBoxW(
                None,
                &windows_strings::HSTRING::from(format!("算法服务启动或运行失败：\n{error}")),
                &windows_strings::HSTRING::from("小狼毫RS"),
                (bindings::MB_OK | bindings::MB_ICONERROR | bindings::MB_SETFOREGROUND) as u32,
            );
        }
        return Err(error);
    }
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("weasel-broker is supported on Windows only");
}
