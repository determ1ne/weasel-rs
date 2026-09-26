//! 为常见文本绘制提供有界缓存，并转发到资源和绘制 API。
//!
//! 缓存按 WASM 线程隔离；字体数达到上限时会清理布局和字体，布局达到上限时会清理
//! 布局。缓存创建资源失败会 panic，因此需要自行处理宿主配额错误或控制缓存策略的主题，
//! 应直接使用 [`crate::resources`] 的 `Result` API。
use crate::{Font, TextLayout};
use std::{cell::RefCell, collections::HashMap};

/// 主题定义的字体槽句柄。
///
/// 槽位编号只用于标识 SDK 内部缓存；主题应保存 [`set_font`] 的返回值，并把该句柄传给
/// 测量和绘制函数，而不是在各处传播裸整数或约定全局槽位含义。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FontSlot(i32);

#[derive(Clone)]
struct FontSpec {
    family: String,
    weight: i32,
}

#[derive(Default)]
struct Cache {
    specs: HashMap<i32, FontSpec>,
    fonts: HashMap<(i32, u32), Font>,
    layouts: HashMap<(i32, u32, String), TextLayout>,
}
thread_local! {static CACHE:RefCell<Cache>=RefCell::new(Cache::default());}

fn with_layout<T>(
    text: &str,
    font: FontSlot,
    size: f32,
    operation: impl FnOnce(&TextLayout) -> T,
) -> T {
    CACHE.with(|cell| {
        let mut cache = cell.borrow_mut();
        let key = (font.0, size.to_bits());
        if !cache.fonts.contains_key(&key) {
            if cache.fonts.len() >= 64 {
                cache.layouts.clear();
                cache.fonts.clear();
            }
            let spec = cache
                .specs
                .get(&font.0)
                .cloned()
                .expect("font slot must be configured with set_font");
            let created =
                Font::new(&spec.family, size, spec.weight).expect("font resource creation");
            cache.fonts.insert(key, created);
        }
        let text_key = (font.0, size.to_bits(), text.to_owned());
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
/// 将任意非负槽位映射到字体族和字重，并返回供测量、绘制使用的强类型句柄。
///
/// 字重范围与 [`Font::new`] 相同，为 `1..=999`。主题通常在创建阶段调用并保存返回值；
/// 再次配置同一槽位会使现有字体和布局缓存失效。
pub fn set_font(slot: i32, family: &str, weight: i32) -> FontSlot {
    assert!(slot >= 0, "font slot must be non-negative");
    assert!((1..=999).contains(&weight), "font weight must be 1..=999");
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        c.layouts.clear();
        c.fonts.clear();
        c.specs.insert(
            slot,
            FontSpec {
                family: family.into(),
                weight,
            },
        );
    });
    FontSlot(slot)
}
/// 测量单行/不换行文本的布局宽度，单位为 DIP；相同文本和参数会复用缓存。
pub fn measure_text(font: FontSlot, text: &str, size: f32) -> f32 {
    with_layout(text, font, size, TextLayout::width)
}
/// 返回用于测量的拉丁字母与中日韩字符样本 `M中` 的布局高度，单位为 DIP。
pub fn line_height(font: FontSlot, size: f32) -> f32 {
    with_layout("M中", font, size, TextLayout::height)
}
/// 使用缓存布局绘制文本，不添加外发光。
pub fn draw_text(font: FontSlot, text: &str, x: f32, y: f32, size: f32, color: u32) {
    draw_text_glow(font, text, x, y, size, color, 0.0, 0);
}
/// 使用缓存布局绘制文本及外发光；颜色采用非预乘 `0xAARRGGBB`。
pub fn draw_text_glow(
    font: FontSlot,
    text: &str,
    x: f32,
    y: f32,
    size: f32,
    color: u32,
    radius: f32,
    glow: u32,
) {
    with_layout(text, font, size, |layout| {
        layout.draw(x, y, color, radius, glow)
    });
}
