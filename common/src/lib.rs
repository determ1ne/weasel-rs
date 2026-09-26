//! Shared messages and RPC transport for weasel-rs.

pub mod about;
#[cfg(windows)]
mod bindings;
pub mod command_menu;
#[cfg(windows)]
pub mod comrt;
pub mod data_frame;
pub mod deploy_protocol;
pub mod logging;
pub mod message;
#[cfg(windows)]
pub mod process;
pub mod rpc;
#[cfg(windows)]
pub mod service_owner;
pub mod settings;
#[cfg(feature = "wasm-metadata")]
pub mod wasm_metadata;
pub mod windows_security;
