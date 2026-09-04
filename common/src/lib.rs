//! Shared messages and RPC transport for weasel-rs.

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
