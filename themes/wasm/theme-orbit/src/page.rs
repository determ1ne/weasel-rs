//! 候选页绘制与翻页过渡：每页携带自己的文字布局和命中数据，翻页时最多保留一张旧页。
use crate::{style::Palette, text::TextCache};
use weasel_wasm_sdk::{
    draw::{FontSlot, draw_text, line_height, pop_draw_state, push_clip, rounded_rect},
    interaction::hit_region,
    layers::{layer_clip, layer_interactive, layer_z_index, with_layer},
};

pub const CURRENT: i32 = 3;
pub const OUTGOING: i32 = 4;
pub const DURATION: f64 = 130.0;

/// 一份可独立绘制的候选页快照。
///
/// `text`、`cells` 与 `enabled` 按候选索引对应；`selected` 保存宿主当前选中项。翻页时旧
/// `Page` 可继续绘制为退场层，而当前页单独接收命中，动画结束后旧页随过渡一起释放。
pub struct Page {
    pub text: TextCache,
    pub cells: Vec<[f32; 4]>,
    pub enabled: Vec<bool>,
    pub selected: usize,
}
impl Page {
    /// 创建文字缓存和空布局；资源创建失败时返回 SDK 错误码。
    pub fn new(size: f32) -> Result<Self, i32> {
        Ok(Self {
            text: TextCache::new(size)?,
            cells: Vec::new(),
            enabled: Vec::new(),
            selected: 0,
        })
    }

    /// 将候选内容绘制到指定图层，并只为当前页登记候选命中区域。
    ///
    /// 图层内容在动画期间保持不变，裁剪、层级和交互状态由宿主合成器承载；`hover` 仅
    /// 改变当前帧的强调样式，不影响候选数据或动画时间线。
    pub fn paint(
        &self,
        id: i32,
        bounds: [f32; 4],
        size: f32,
        number_font: FontSlot,
        p: &Palette,
        hover: i32,
    ) {
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
                draw_text(
                    number_font,
                    &(i + 1).to_string(),
                    x + 8.0,
                    y + (h - line_height(number_font, small)) / 2.0,
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

/// 页面切换期间保留的退场页及其连续动画参数。
///
/// 新一轮翻页可从旧过渡当前呈现的位置接续；`direction` 决定进入方向，`distance` 在新页
/// 布局确定后固定，避免后续快照改变运动跨度。
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
    /// 计算带缓出曲线的归一化进度，并限制在完整过渡区间内。
    pub fn progress(&self, now: f64) -> f32 {
        let t = ((now - self.start) / DURATION).clamp(0.0, 1.0) as f32;
        1.0 - (1.0 - t).powi(2)
    }
    /// 返回过渡中当前页应有的横向位置和透明度。
    pub fn incoming(&self, now: f64) -> (f32, f32) {
        let t = self.progress(now);
        (self.direction * self.distance * (1.0 - t), 1.0)
    }
}
