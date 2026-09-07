//! Windows TSF COM server. Engine and UI work stay out of the host process.
#![allow(non_snake_case)]

mod bindings;
mod boundary;
mod class_factory;
mod diagnostics;
mod icons;
mod keyboard;
mod module;
mod registration;
mod rpc_diagnostics;
mod rpc_worker;
mod service;
mod update_window;

pub use bindings::CLSID_WEASEL_TIP;
pub use class_factory::{
    DllCanUnloadNow, DllGetClassObject, DllRegisterServer, DllUnregisterServer,
};
