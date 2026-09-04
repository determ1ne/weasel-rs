#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod bindings;
#[cfg(windows)]
mod diagnostics;
#[cfg(windows)]
mod rpc;
mod state;
#[cfg(windows)]
mod theme;
#[cfg(windows)]
mod xaml_host;

#[cfg(windows)]
fn main() {
    if weasel_common::runtime_paths::is_development() {
        unsafe {
            let _ = bindings::Windows::Win32::AllocConsole();
        }
    }
    let _instance = match weasel_common::process::SingleInstance::acquire("renderer") {
        Ok(instance) => instance,
        Err(error) => {
            eprintln!("weasel-renderer: {error}");
            return;
        }
    };
    diagnostics::initialize();
    diagnostics::record(format_args!("starting"));
    if let Err(error) = rpc::run() {
        diagnostics::record(format_args!("{error}"));
        std::process::exit(1);
    }
    diagnostics::record(format_args!("stopped"));
}

#[cfg(not(windows))]
fn main() {
    eprintln!("weasel-renderer is supported on Windows only");
}
