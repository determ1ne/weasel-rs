#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod appearance;
#[cfg(windows)]
mod backend;
#[cfg(windows)]
mod bindings;
#[cfg(windows)]
mod d2d_bindings;
#[cfg(windows)]
mod diagnostics;
#[cfg(windows)]
mod presentation;
#[cfg(windows)]
mod preview;
#[cfg(windows)]
mod preview_window;
#[cfg(windows)]
mod rpc;
mod state;
#[cfg(windows)]
mod theme_abc;
#[cfg(windows)]
mod theme_adapter;
mod theme_api;
#[cfg(windows)]
mod theme_eleven;
#[cfg(windows)]
mod theme_ten;
#[cfg(windows)]
mod ui_runtime;

#[cfg(windows)]
fn main() {
    if weasel_common::runtime_paths::is_development() {
        unsafe {
            let _ = bindings::Windows::Win32::AllocConsole();
        }
    }
    let preview = match preview::parse_args(std::env::args_os().skip(1)) {
        Ok(preview) => preview,
        Err(error) => {
            eprintln!("weasel-renderer: {error}");
            std::process::exit(1);
        }
    };
    let component = if preview != theme_api::UiMode::Live {
        "renderer-preview"
    } else {
        "renderer"
    };
    let _instance = match weasel_common::process::SingleInstance::acquire(component) {
        Ok(instance) => instance,
        Err(error) => {
            eprintln!("weasel-renderer: {error}");
            return;
        }
    };
    diagnostics::initialize();
    diagnostics::record(format_args!("starting (mode={preview:?})"));
    let result = if preview != theme_api::UiMode::Live {
        preview::run(preview)
    } else {
        rpc::run()
    };
    if let Err(error) = result {
        diagnostics::record(format_args!("{error}"));
        std::process::exit(1);
    }
    diagnostics::record(format_args!("stopped"));
}

#[cfg(not(windows))]
fn main() {
    eprintln!("weasel-renderer is supported on Windows only");
}
