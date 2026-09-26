//! Windows TSF 文本服务的 COM DLL 入口。
//!
//! 本模块导出 COM 生命周期与注册函数；服务通过独立 RPC 工作线程连接输入引擎，窗口消息、
//! 编辑会话及 TSF 对象仍遵守宿主要求的 apartment 线程边界。
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
