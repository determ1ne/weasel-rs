//! 由构建脚本生成并包含 Windows UI/XAML API 绑定；此模块不手工维护绑定声明。
#![allow(
    dead_code,
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    clippy::all
)]
include!(concat!(env!("OUT_DIR"), "/ui_bindings.rs"));
