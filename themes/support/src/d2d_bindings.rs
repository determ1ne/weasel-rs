#![allow(
    dead_code,
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    clippy::all
)]
include!(concat!(env!("OUT_DIR"), "/d2d-bindings.rs"));
pub use Windows::Win32::*;
