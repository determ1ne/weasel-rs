//! 有界简便绘图缓存。需要自行处理配额失败时直接使用返回Result的资源API。
use crate::{Font, TextLayout};
use std::{cell::RefCell, collections::HashMap};
#[derive(Default)]
struct Cache {
    families: HashMap<i32, String>,
    fonts: HashMap<(i32, u32), Font>,
    layouts: HashMap<(i32, u32, String), TextLayout>,
}
thread_local! {static CACHE:RefCell<Cache>=RefCell::new(Cache::default());}
fn with_layout<T>(text: &str, font: i32, size: f32, operation: impl FnOnce(&TextLayout) -> T) -> T {
    CACHE.with(|cell| {
        let mut cache = cell.borrow_mut();
        let key = (font, size.to_bits());
        if !cache.fonts.contains_key(&key) {
            if cache.fonts.len() >= 64 {
                cache.layouts.clear();
                cache.fonts.clear();
            }
            let family_slot = if font == 4 { 0 } else { font };
            let family = cache
                .families
                .get(&family_slot)
                .map(String::as_str)
                .unwrap_or(match font {
                    1 => "Segoe UI",
                    3 => "Segoe MDL2 Assets",
                    _ => "Microsoft YaHei UI",
                });
            let created = Font::new(family, size, if font == 4 { 700 } else { 400 })
                .expect("font resource creation");
            cache.fonts.insert(key, created);
        }
        let text_key = (font, size.to_bits(), text.to_owned());
        if !cache.layouts.contains_key(&text_key) {
            if cache.layouts.len() >= 128 {
                cache.layouts.clear();
            }
            let created = TextLayout::new(&cache.fonts[&key], text, 65536.0, 4096.0, false)
                .expect("text resource creation");
            cache.layouts.insert(text_key.clone(), created);
        }
        operation(&cache.layouts[&text_key])
    })
}
pub fn set_font(slot: i32, family: &str) {
    assert!((0..=3).contains(&slot));
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        c.layouts.clear();
        c.fonts.clear();
        c.families.insert(slot, family.into());
    });
}
pub fn measure(text: &str, font: i32, size: f32) -> f32 {
    with_layout(text, font, size, TextLayout::width)
}
pub fn line_height(font: i32, size: f32) -> f32 {
    with_layout("M中", font, size, TextLayout::height)
}
pub fn draw(text: &str, x: f32, y: f32, font: i32, size: f32, color: u32) {
    draw_glow(text, x, y, font, size, color, 0.0, 0);
}
pub fn draw_glow(
    text: &str,
    x: f32,
    y: f32,
    font: i32,
    size: f32,
    color: u32,
    radius: f32,
    glow: u32,
) {
    with_layout(text, font, size, |layout| {
        layout.draw(x, y, color, radius, glow)
    });
}
