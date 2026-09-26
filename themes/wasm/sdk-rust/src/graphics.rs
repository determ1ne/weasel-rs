//! 为常见文本绘制提供有界缓存，并转发到资源和绘制 API。
//!
//! 缓存按 WASM 线程隔离；字体数达到上限时会清理布局和字体，布局达到上限时会清理
//! 布局。缓存创建资源失败会 panic，因此需要自行处理宿主配额错误或控制缓存策略的主题，
//! 应直接使用 [`crate::resources`] 的 `Result` API。
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
/// 将字体槽位 `0..=3` 映射到字体族名称，并清空派生的字体与文本布局缓存。
///
/// 槽位 4 固定表示粗体，不可通过此函数配置；常在创建阶段设置，避免动画事件反复失效缓存。
pub fn set_font(slot: i32, family: &str) {
    assert!((0..=3).contains(&slot));
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        c.layouts.clear();
        c.fonts.clear();
        c.families.insert(slot, family.into());
    });
}
/// 测量单行/不换行文本的布局宽度，单位为 DIP；相同文本和参数会复用缓存。
pub fn measure(text: &str, font: i32, size: f32) -> f32 {
    with_layout(text, font, size, TextLayout::width)
}
/// 返回用于测量的拉丁字母与中日韩字符样本 `M中` 的布局高度，单位为 DIP。
pub fn line_height(font: i32, size: f32) -> f32 {
    with_layout("M中", font, size, TextLayout::height)
}
/// 使用缓存布局绘制文本，不添加外发光。
pub fn draw(text: &str, x: f32, y: f32, font: i32, size: f32, color: u32) {
    draw_glow(text, x, y, font, size, color, 0.0, 0);
}
/// 使用缓存布局绘制文本及外发光；颜色采用非预乘 `0xAARRGGBB`。
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
