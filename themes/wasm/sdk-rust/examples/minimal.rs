//! Minimal theme using the standalone Rust guest SDK.
use weasel_wasm_sdk::{ABI_VERSION, CANDIDATE, draw, fill_rect, set_size};

#[unsafe(no_mangle)]
pub extern "C" fn abi_version() -> i32 {
    ABI_VERSION
}
#[unsafe(no_mangle)]
pub extern "C" fn init(_mode: i32, _dark: i32) -> i32 {
    0
}
#[unsafe(no_mangle)]
pub extern "C" fn render() -> i32 {
    let text = CANDIDATE
        .string("/items/0/primary_text")
        .unwrap_or_default();
    // OPTIONS.string("/palette/label") reads theme-specific settings.
    {
        set_size(240.0, 48.0);
        fill_rect(0.0, 0.0, 240.0, 48.0, 0xff202020);
        draw(&text, 8.0, 8.0, 0, 20.0, 0xffffffff);
    }
    0
}
#[unsafe(no_mangle)]
pub extern "C" fn mouse(_kind: i32, _x: f32, _y: f32) {}
#[unsafe(no_mangle)]
pub extern "C" fn frame(_now: f64) {}
#[unsafe(no_mangle)]
pub extern "C" fn hide() {}
#[unsafe(no_mangle)]
pub extern "C" fn refresh(_dark: i32) {}
