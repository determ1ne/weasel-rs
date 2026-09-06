use weasel_common::message::{RenderSnapshot, RendererEvent, RendererEventAction};

pub const SCALE: f32 = 46.0 / 68.0;
pub const HEIGHT: f32 = 46.0;
pub const NUMBER: f32 = 40.0 * SCALE;
pub const PAD: f32 = 18.0 * SCALE;

#[derive(Default)]
pub struct Recovery {
    failures: u8,
    pub waiting: bool,
}
impl Recovery {
    pub fn failed(&mut self, device_lost: bool) -> bool {
        if !device_lost || self.failures >= 3 {
            return false;
        }
        self.failures += 1;
        self.waiting = true;
        true
    }
    pub fn succeeded(&mut self) {
        self.failures = 0;
        self.waiting = false;
    }
}

pub fn pixels(dip: f32, dpi: u32) -> i32 {
    (dip * dpi.max(1) as f32 / 96.0).round().max(1.0) as i32
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Candidate(usize),
    Previous,
    Next,
    Emoji,
}

#[derive(Clone, Copy, Debug)]
pub struct Cell {
    pub hit: Hit,
    pub left: f32,
    pub right: f32,
}

#[derive(Default)]
pub struct Layout {
    pub cells: Vec<Cell>,
    pub width: f32,
}

impl Layout {
    /// Widths are measured by DirectWrite in DIPs, including inline comments.
    pub fn new(widths: impl IntoIterator<Item = f32>) -> Self {
        let mut result = Self::default();
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
    fn push(&mut self, hit: Hit, width: f32) {
        self.cells.push(Cell {
            hit,
            left: self.width,
            right: self.width + width,
        });
        self.width += width;
    }
    pub fn hit(&self, x: f32, y: f32) -> Option<Hit> {
        if !(0.0..HEIGHT).contains(&y) {
            return None;
        }
        self.cells
            .iter()
            .find(|c| x >= c.left && x < c.right)
            .map(|c| c.hit)
    }
}

pub fn enabled(snapshot: &RenderSnapshot, hit: Hit) -> bool {
    snapshot.visible
        && match hit {
            Hit::Candidate(i) => snapshot.items.get(i).is_some_and(|item| item.enabled),
            Hit::Previous => snapshot.can_page_previous,
            Hit::Next => snapshot.can_page_next,
            Hit::Emoji => true,
        }
}

#[derive(Default)]
pub struct Gesture {
    pub pressed: Option<Hit>,
    pub hovered: Option<Hit>,
}
impl Gesture {
    pub fn cancel(&mut self) {
        self.pressed = None;
        self.hovered = None;
    }
    pub fn press(&mut self, hit: Option<Hit>, snapshot: &RenderSnapshot) {
        self.pressed = hit.filter(|h| enabled(snapshot, *h));
    }
    pub fn motion(&mut self, hit: Option<Hit>) {
        self.hovered = hit;
        // Once the pointer leaves its pressed target, returning cannot invoke it.
        if self.pressed != hit {
            self.pressed = None;
        }
    }
    pub fn release(
        &mut self,
        hit: Option<Hit>,
        snapshot: &RenderSnapshot,
    ) -> Option<RendererEvent> {
        let pressed = self.pressed.take()?;
        if Some(pressed) != hit || !enabled(snapshot, pressed) {
            return None;
        }
        let (action, item_index) = match pressed {
            Hit::Candidate(i) => (RendererEventAction::ItemInvoked, i as u32),
            Hit::Previous => (RendererEventAction::NavigatePrevious, 0),
            Hit::Next => (RendererEventAction::NavigateNext, 0),
            Hit::Emoji => (RendererEventAction::OpenEmojiPanel, 0),
        };
        Some(RendererEvent {
            session_id: snapshot.session_id,
            token: snapshot.token.clone(),
            revision: snapshot.revision,
            action: action as i32,
            item_index,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub background: u32,
    pub border: u32,
    pub active: u32,
    pub hover: u32,
    pub text: u32,
    pub secondary: u32,
    pub active_number: u32,
    pub disabled: u32,
}
impl Palette {
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
    use weasel_common::message::{ContextToken, RenderItem};
    fn snapshot() -> RenderSnapshot {
        RenderSnapshot {
            visible: true,
            session_id: 42,
            revision: 7,
            token: Some(ContextToken::default()),
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
        assert_eq!((e.session_id, e.revision, e.item_index), (42, 7, 0));
        assert_eq!(e.token, s.token);
        s.items[0].enabled = false;
        g.press(h, &s);
        assert!(g.release(h, &s).is_none());
        assert!(!enabled(&s, Hit::Previous));
        assert!(!enabled(&s, Hit::Next));
        s.can_page_next = true;
        assert!(enabled(&s, Hit::Next));
        g.press(Some(Hit::Next), &s);
        assert_eq!(
            g.release(Some(Hit::Next), &s).unwrap().action,
            RendererEventAction::NavigateNext as i32
        );
        g.press(Some(Hit::Emoji), &s);
        assert_eq!(
            g.release(Some(Hit::Emoji), &s).unwrap().action,
            RendererEventAction::OpenEmojiPanel as i32
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
