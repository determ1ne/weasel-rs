//! 候选页拥有布局缓存；翻页最多保留一个退场页，不积压动画或候选资源。
use crate::{style::Palette, text::TextCache};
use weasel_wasm_sdk::{
    draw::{draw, line_height, pop_draw_state, push_clip, rounded_rect},
    interaction::hit_region,
    layers::{layer_clip, layer_interactive, layer_z_index, with_layer},
};

pub const CURRENT: i32 = 3;
pub const OUTGOING: i32 = 4;
pub const DURATION: f64 = 130.0;

pub struct Page {
    pub text: TextCache,
    pub cells: Vec<[f32; 4]>,
    pub enabled: Vec<bool>,
    pub selected: usize,
}
impl Page {
    pub fn new(size: f32) -> Result<Self, i32> {
        Ok(Self {
            text: TextCache::new(size)?,
            cells: Vec::new(),
            enabled: Vec::new(),
            selected: 0,
        })
    }

    pub fn paint(&self, id: i32, bounds: [f32; 4], size: f32, p: &Palette, hover: i32) {
        let [left, top, width, height] = bounds;
        // 内容只绘制一次，位移/透明度由合成器推进。裁剪属于静止父容器。
        with_layer(id, left + width, top + height, || {
            let small = (size - 3.0).max(11.0);
            for (i, (text, cell)) in self.text.items.iter().zip(&self.cells).enumerate() {
                let [x, y, w, h] = *cell;
                let enabled = self.enabled[i];
                let selected = i == self.selected && enabled;
                if selected || (hover == i as i32 + 1 && enabled) {
                    let alpha = if hover == i as i32 + 1 {
                        0x28000000
                    } else {
                        0x14000000
                    };
                    rounded_rect(x, y, w, h, 8.0, (p.accent & 0xffffff) | alpha);
                }
                if selected {
                    rounded_rect(x + 8.0, y + h - 4.0, w - 16.0, 2.0, 1.0, p.accent);
                }
                push_clip([x + 5.0, y, w - 10.0, h]);
                draw(
                    &(i + 1).to_string(),
                    x + 8.0,
                    y + (h - line_height(1, small)) / 2.0,
                    1,
                    small,
                    p.muted,
                );
                text.primary.layout.draw(
                    x + 30.0,
                    y + (h - text.primary.layout.height()) / 2.0,
                    if enabled { p.text } else { p.muted },
                    0.0,
                    0,
                );
                text.secondary.layout.draw(
                    x + 40.0 + text.primary.layout.width(),
                    y + (h - text.secondary.layout.height()) / 2.0,
                    p.muted,
                    0.0,
                    0,
                );
                pop_draw_state();
                if enabled && id == CURRENT {
                    hit_region(i as i32 + 1, x, y, w, h, 8.0);
                }
            }
        });
        layer_clip(id, Some(bounds));
        layer_z_index(id, if id == CURRENT { 10 } else { 0 });
        layer_interactive(id, id == CURRENT);
    }
}

pub struct Transition {
    pub old: Page,
    pub start: f64,
    pub direction: f32,
    /// 按整行可视宽度计算，避免短距离交叉淡化造成逐项切换的观感。
    pub distance: f32,
    pub old_offset: f32,
    pub old_opacity: f32,
}
impl Transition {
    pub fn progress(&self, now: f64) -> f32 {
        let t = ((now - self.start) / DURATION).clamp(0.0, 1.0) as f32;
        1.0 - (1.0 - t).powi(2)
    }
    pub fn incoming(&self, now: f64) -> (f32, f32) {
        let t = self.progress(now);
        (self.direction * self.distance * (1.0 - t), 1.0)
    }
}
