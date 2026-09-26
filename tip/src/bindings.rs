//! 由构建脚本从 Windows SDK 生成的 Win32、COM 与 TSF 绑定。
//! 本模块集中承载 FFI 声明；具体生成内容位于 `OUT_DIR`，不在此手工维护。
#![allow(dead_code)]
#![allow(non_camel_case_types)]
#![allow(non_upper_case_globals)]

include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
