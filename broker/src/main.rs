//! The per-user broker: owns the tray icon and the server/renderer children.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod child_process;
mod installer;
mod lifecycle;
mod managed_children;
mod notifications;
mod service_rpc;
mod settings;
mod settings_rpc;
#[cfg(windows)]
mod shortcut;
mod shutdown;
mod toast;
#[cfg(windows)]
mod updater;

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
    let command = args.next();
    if command.as_deref() == Some(std::ffi::OsStr::new("--shutdown")) {
        let directory = args.next().map(std::path::PathBuf::from).unwrap_or(
            std::env::current_exe()?
                .parent()
                .ok_or("missing executable directory")?
                .to_owned(),
        );
        if args.next().is_some() {
            return Err("unexpected shutdown arguments".into());
        }
        return shutdown::run(&directory);
    }
    if command.as_deref() == Some(std::ffi::OsStr::new("--install-shortcut")) {
        let path = args.next().ok_or("missing shortcut path")?;
        if args.next().is_some() {
            return Err("unexpected shortcut arguments".into());
        }
        return shortcut::install(std::path::Path::new(&path));
    }
    if command.as_deref() == Some(std::ffi::OsStr::new("--post-install")) {
        if args.next().is_some() {
            return Err("unexpected post-install arguments".into());
        }
        return installer::post_install();
    }
    if command.as_deref() == Some(std::ffi::OsStr::new("--post-uninstall")) {
        if args.next().is_some() {
            return Err("unexpected post-uninstall arguments".into());
        }
        return installer::post_uninstall();
    }
    if command.is_some() {
        return Err("unknown broker option".into());
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
