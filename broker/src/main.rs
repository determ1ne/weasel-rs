//! The per-user broker: owns the tray icon and the server/renderer children.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod lifecycle;
mod notifications;
mod settings;
mod settings_rpc;
mod toast;

#[cfg(windows)]
mod bindings;

#[cfg(windows)]
mod menu_actions;
#[cfg(windows)]
mod windows_tray;

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    if weasel_common::runtime_paths::is_development() {
        unsafe {
            let _ = bindings::AllocConsole();
        }
    }
    windows_tray::run()
}

#[cfg(not(windows))]
fn main() {
    eprintln!("weasel-broker is supported on Windows only");
}
