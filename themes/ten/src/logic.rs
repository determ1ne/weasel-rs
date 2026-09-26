//! ten 候选栏的纯布局、命中测试、配色和鼠标手势逻辑。

use crate::theme_api::{CandidateView, UiAction};

/// 设计基准到 DIP 的缩放比例。
pub const SCALE: f32 = 46.0 / 68.0;
/// 候选栏在设计坐标中的高度，单位为 DIP。
pub const HEIGHT: f32 = 46.0;
/// 中英文模式提示方框的边长，单位为 DIP。
pub const INDICATOR_SIZE: f32 = 36.0;
/// 候选序号列宽，单位为 DIP。
pub const NUMBER: f32 = 40.0 * SCALE;
/// 候选项右侧留白，单位为 DIP。
pub const PAD: f32 = 18.0 * SCALE;

/// Direct2D 设备丢失后的有限重试状态。
///
/// 每次失败最多安排一次延迟重绘；成功绘制会清零连续失败计数。
#[derive(Default)]
pub struct Recovery {
    /// 当前连续设备丢失次数；预算上限为三次。
    failures: u8,
    /// 定时重试尚未触发时为真，窗口过程据此暂缓绘制。
    pub waiting: bool,
}
impl Recovery {
    /// 记录失败；仅设备丢失且尚有预算时返回 `true` 并进入等待状态。
    pub fn failed(&mut self, device_lost: bool) -> bool {
        if !device_lost || self.failures >= 3 {
            return false;
        }
        self.failures += 1;
        self.waiting = true;
        true
    }
    /// 记录一次成功绘制并重置失败预算与等待状态。
    pub fn succeeded(&mut self) {
        self.failures = 0;
        self.waiting = false;
    }
}

/// 将 DIP 长度转换为物理像素，DPI 至少按 1 处理，结果至少为 1 像素。
pub fn pixels(dip: f32, dpi: u32) -> i32 {
    (dip * dpi.max(1) as f32 / 96.0).round().max(1.0) as i32
}

/// 候选栏中可被鼠标命中的语义区域。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    /// 从零开始的候选项索引。
    Candidate(usize),
    /// 请求上一页候选项。
    Previous,
    /// 请求下一页候选项。
    Next,
    /// 打开表情面板。
    Emoji,
}

/// 单个可命中区域的横向 DIP 边界。
#[derive(Clone, Copy, Debug)]
pub struct Cell {
    /// 此区域对应的候选项或操作。
    pub hit: Hit,
    /// 左边界，包含。
    pub left: f32,
    /// 右边界，不包含。
    pub right: f32,
}

/// 按绘制顺序排列的栏位及其总宽度，全部以 DIP 表示。
#[derive(Default)]
pub struct Layout {
    /// 候选项及固定操作区；相邻区域首尾相接。
    pub cells: Vec<Cell>,
    /// 所有区域宽度之和。
    pub width: f32,
    /// 当前窗口高度；模式提示可独立于候选栏使用更紧凑的尺寸。
    pub height: f32,
}

impl Layout {
    /// 为无候选的中英文模式提示创建一个不可交互的紧凑栏位。
    pub fn mode_indicator() -> Self {
        Self {
            cells: Vec::new(),
            width: INDICATOR_SIZE,
            height: INDICATOR_SIZE,
        }
    }

    /// 根据候选项的测量宽度构造布局。
    ///
    /// 每项宽度受最小宽度约束，随后追加上一页、下一页和表情操作区；输入宽度应与
    /// 候选项顺序一致，且已包含主文本和行内注释占用的宽度。
    pub fn new(widths: impl IntoIterator<Item = f32>) -> Self {
        let mut result = Self {
            height: HEIGHT,
            ..Default::default()
        };
        for (index, text) in widths.into_iter().enumerate() {
            result.push(
                Hit::Candidate(index),
                (93.0 * SCALE).max(NUMBER + text.ceil() + PAD),
            );
        }
        result.push(Hit::Previous, 49.0 * SCALE);
        result.push(Hit::Next, 49.0 * SCALE);
        result.push(Hit::Emoji, 74.0 * SCALE);
        result
    }
    /// 在末尾追加一个区域，并同步扩展总宽度。
    fn push(&mut self, hit: Hit, width: f32) {
        self.cells.push(Cell {
            hit,
            left: self.width,
            right: self.width + width,
        });
        self.width += width;
    }
    /// 按左闭右开的横向边界和栏高查找命中区域。
    ///
    /// 超出栏高、区域范围或位于右边界上的点均返回 `None`；查找按区域线性扫描。
    pub fn hit(&self, x: f32, y: f32) -> Option<Hit> {
        if !(0.0..self.height).contains(&y) {
            return None;
        }
        self.cells
            .iter()
            .find(|c| x >= c.left && x < c.right)
            .map(|c| c.hit)
    }
}

/// 根据快照判断命中目标当前是否允许触发。
///
/// 隐藏快照禁用所有目标；候选项依其自身 `enabled` 标记，翻页依分页能力标记，
/// 表情操作只受整体可见状态约束。
pub fn enabled(snapshot: &CandidateView, hit: Hit) -> bool {
    snapshot.visible
        && match hit {
            Hit::Candidate(i) => snapshot.items.get(i).is_some_and(|item| item.enabled),
            Hit::Previous => snapshot.can_page_previous,
            Hit::Next => snapshot.can_page_next,
            Hit::Emoji => true,
        }
}

/// 按下、移动、释放组成的鼠标手势状态。
///
/// 按下目标一旦因指针离开而取消，移回不会恢复；只有在同一有效目标上释放才产生动作。
#[derive(Default)]
pub struct Gesture {
    /// 当前仍有效的按下目标。
    pub pressed: Option<Hit>,
    /// 指针当前位置对应的目标，用于悬停绘制。
    pub hovered: Option<Hit>,
}
impl Gesture {
    /// 清空按下与悬停状态，用于失焦、捕获丢失或内容替换。
    pub fn cancel(&mut self) {
        self.pressed = None;
        self.hovered = None;
    }
    /// 仅当目标在当前快照中可用时记录按下状态。
    pub fn press(&mut self, hit: Option<Hit>, snapshot: &CandidateView) {
        self.pressed = hit.filter(|h| enabled(snapshot, *h));
    }
    /// 更新悬停目标；离开按下目标后永久取消本次点击。
    pub fn motion(&mut self, hit: Option<Hit>) {
        self.hovered = hit;
        // Once the pointer leaves its pressed target, returning cannot invoke it.
        if self.pressed != hit {
            self.pressed = None;
        }
    }
    /// 完成手势；仅同一仍启用的目标返回对应 UI 动作。
    ///
    /// 无有效按下、按下与释放目标不一致或目标已禁用时均返回 `None`；无论是否成功，
    /// 本次按下状态都会被消费。
    pub fn release(&mut self, hit: Option<Hit>, snapshot: &CandidateView) -> Option<UiAction> {
        let pressed = self.pressed.take()?;
        if Some(pressed) != hit || !enabled(snapshot, pressed) {
            return None;
        }
        Some(match pressed {
            Hit::Candidate(i) => UiAction::ItemInvoked(i as u32),
            Hit::Previous => UiAction::NavigatePrevious,
            Hit::Next => UiAction::NavigateNext,
            Hit::Emoji => UiAction::OpenEmojiPanel,
        })
    }
}

/// 亮色或暗色主题使用的 RGB 色板，颜色以 `0xRRGGBB` 表示。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    /// 栏体背景色。
    pub background: u32,
    /// 外框及分隔线颜色。
    pub border: u32,
    /// 当前选中项的背景色。
    pub active: u32,
    /// 鼠标悬停背景色。
    pub hover: u32,
    /// 主文本颜色。
    pub text: u32,
    /// 次要文本颜色。
    pub secondary: u32,
    /// 选中项序号颜色。
    pub active_number: u32,
    /// 禁用内容颜色。
    pub disabled: u32,
}
impl Palette {
    /// 根据系统外观选择一组完整色板。
    pub fn new(dark: bool) -> Self {
        if dark {
            Self {
                background: 0x202020,
                border: 0x484848,
                active: 0x164E70,
                hover: 0x383838,
                text: 0xF5F5F5,
                secondary: 0xBEBEBE,
                active_number: 0xD6EDFF,
                disabled: 0x777777,
            }
        } else {
            Self {
                background: 0xF8F8F8,
                border: 0xD9D9D9,
                active: 0xA6D8FF,
                hover: 0xDCDCDC,
                text: 0x111111,
                secondary: 0x555555,
                active_number: 0x3E4A52,
                disabled: 0xA5A5A5,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme_api::CandidateItem as RenderItem;
    fn snapshot() -> CandidateView {
        CandidateView {
            visible: true,
            content_id: 7,
            items: vec![RenderItem {
                enabled: true,
                ..Default::default()
            }],
            ..Default::default()
        }
    }
    #[test]
    fn height_at_display_scales() {
        assert_eq!(
            [pixels(HEIGHT, 96), pixels(HEIGHT, 144), pixels(HEIGHT, 192)],
            [46, 69, 92]
        );
        let indicator = Layout::mode_indicator();
        assert_eq!((indicator.width, indicator.height), (36.0, 36.0));
        assert_eq!(
            [
                pixels(indicator.height, 96),
                pixels(indicator.height, 144),
                pixels(indicator.height, 192),
            ],
            [36, 54, 72]
        );
    }
    #[test]
    fn recovery_is_bounded_and_success_resets_budget() {
        let mut r = Recovery::default();
        for _ in 0..3 {
            assert!(r.failed(true));
            assert!(r.waiting);
            r.waiting = false;
        }
        assert!(!r.failed(true));
        r.succeeded();
        assert!(r.failed(true));
        r.succeeded();
        assert!(!r.failed(false));
        assert!(!r.waiting);
    }
    #[test]
    fn dynamic_layout_and_shared_hit_coordinates() {
        for n in [0, 1, 7, 256] {
            let l = Layout::new(vec![10.0; n]);
            assert_eq!(l.cells.len(), n + 3);
            for c in &l.cells {
                assert_eq!(l.hit(c.left, 0.0), Some(c.hit));
                for dpi in [96, 144, 192] {
                    let px = (c.left + c.right) / 2.0 * dpi as f32 / 96.0;
                    assert_eq!(l.hit(px * 96.0 / dpi as f32, 23.0), Some(c.hit));
                }
            }
            assert_eq!(l.hit(l.width, 20.0), None);
            assert_eq!(l.hit(0.0, HEIGHT), None);
            assert_eq!(l.hit(-1.0, 0.0), None);
        }
    }
    #[test]
    fn long_text_and_inline_comment_expand_cell() {
        let short = Layout::new([10.0]);
        let long = Layout::new([1200.0 + 300.0 + 8.0 * SCALE]);
        assert!(long.cells[0].right > short.cells[0].right);
        assert!(long.cells[0].right >= NUMBER + 1500.0 + PAD);
        assert_eq!(long.cells[1].left, long.cells[0].right);
    }
    #[test]
    fn click_requires_enabled_matching_press_and_release() {
        let mut s = snapshot();
        let mut g = Gesture::default();
        let h = Some(Hit::Candidate(0));
        assert!(g.release(h, &s).is_none());
        g.press(h, &s);
        let e = g.release(h, &s).unwrap();
        assert_eq!(e, UiAction::ItemInvoked(0));
        s.items[0].enabled = false;
        g.press(h, &s);
        assert!(g.release(h, &s).is_none());
        assert!(!enabled(&s, Hit::Previous));
        assert!(!enabled(&s, Hit::Next));
        s.can_page_next = true;
        assert!(enabled(&s, Hit::Next));
        g.press(Some(Hit::Next), &s);
        assert_eq!(
            g.release(Some(Hit::Next), &s).unwrap(),
            UiAction::NavigateNext
        );
        g.press(Some(Hit::Emoji), &s);
        assert_eq!(
            g.release(Some(Hit::Emoji), &s).unwrap(),
            UiAction::OpenEmojiPanel
        );
    }
    #[test]
    fn outside_captureloss_hide_and_replacement_cancel() {
        let s = snapshot();
        let h = Some(Hit::Candidate(0));
        let mut g = Gesture::default();
        g.press(h, &s);
        g.motion(None);
        g.motion(h);
        assert!(g.release(h, &s).is_none());
        for _ in 0..3 {
            g.press(h, &s);
            g.cancel();
            assert!(g.release(h, &s).is_none());
        }
        g.press(h, &s);
        assert!(g.release(Some(Hit::Emoji), &s).is_none());
        let mut hidden = s.clone();
        hidden.visible = false;
        g.press(h, &s);
        assert!(g.release(h, &hidden).is_none());
    }
    #[test]
    fn light_dark_palettes_have_distinct_states() {
        assert_ne!(Palette::new(false), Palette::new(true));
        for p in [Palette::new(false), Palette::new(true)] {
            assert_ne!(p.background, p.text);
            assert_ne!(p.active, p.hover);
            assert_ne!(p.disabled, p.text);
            assert_ne!(p.secondary, p.text);
        }
    }
}
