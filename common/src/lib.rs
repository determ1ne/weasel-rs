//! Shared messages and RPC transport for weasel-rs.

pub mod about;
#[cfg(windows)]
mod bindings;
pub mod broker_menu;
pub mod deploy_protocol;
pub mod framing;
pub mod input_diagnostics;
pub mod logging;
pub mod message;
pub mod platform;
#[cfg(windows)]
pub mod process;
pub mod rpc;
pub mod runtime_paths;
#[cfg(windows)]
pub mod service_owner;
pub mod settings;
