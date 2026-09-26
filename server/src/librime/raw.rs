//! 由构建脚本生成的 librime C ABI 类型与函数表声明；字段布局以生成绑定为准。

#![allow(non_camel_case_types, non_snake_case, non_upper_case_globals)]
#![allow(clippy::all)]

include!(concat!(env!("OUT_DIR"), "/rime_bindings.rs"));
