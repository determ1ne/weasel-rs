//! Resource-free classic candidate geometry. All dimensions are DIPs.
use crate::theme_api::{CandidateView, UiAction};
pub const ROW: f32 = 16.0;
pub const PAD: f32 = 4.0;
pub const INPUT_WIDTH: f32 = 173.0;
pub const INPUT_HEIGHT: f32 = 26.0;
pub const CANDIDATE_WIDTH: f32 = 127.0;
pub const GAP: f32 = 8.0;
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Input,
    Candidates,
}

/// Candidate window follows the input window, flipping to its left at the edge.
pub fn candidate_position(
    input: (i32, i32),
    input_width: i32,
    size: (i32, i32),
    gap: i32,
    work: &crate::theme_api::Anchor,
) -> (i32, i32) {
    let right = input.0.saturating_add(input_width).saturating_add(gap);
    let x = if right.saturating_add(size.0) > work.right {
        input.0.saturating_sub(size.0).saturating_sub(gap)
    } else {
        right
    };
    (
        x.min(work.right.saturating_sub(size.0)).max(work.left),
        input
            .1
            .min(work.bottom.saturating_sub(size.1))
            .max(work.top),
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Candidate(usize),
    Previous,
    Next,
    PreviousDecorative,
    NextDecorative,
}
#[derive(Clone, Copy, Debug)]
pub struct Cell {
    pub hit: Hit,
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}
#[derive(Default)]
pub struct Layout {
    pub cells: Vec<Cell>,
    pub width: f32,
    pub height: f32,
}
impl Layout {
    pub fn new(count: usize, role: Role) -> Self {
        if role == Role::Input {
            return Self {
                cells: Vec::new(),
                width: INPUT_WIDTH,
                height: INPUT_HEIGHT,
            };
        }
        let rows = count.max(9);
        let text_bottom = ROW * rows as f32 + 1.0;
        let mut cells = Vec::new();
        for i in 0..count {
            cells.push(Cell {
                hit: Hit::Candidate(i),
                left: PAD,
                top: PAD + i as f32 * ROW,
                right: 112.0,
                bottom: PAD + (i + 1) as f32 * ROW,
            });
        }
        for (hit, left) in [
            (Hit::PreviousDecorative, 4.0),
            (Hit::NextDecorative, 18.0),
            (Hit::Previous, 95.0),
            (Hit::Next, 109.0),
        ] {
            cells.push(Cell {
                hit,
                left,
                top: text_bottom + 6.0,
                right: left + 14.0,
                bottom: text_bottom + 20.0,
            });
        }
        Self {
            cells,
            width: CANDIDATE_WIDTH,
            height: text_bottom + 24.0,
        }
    }
    pub fn hit(&self, x: f32, y: f32) -> Option<Hit> {
        self.cells
            .iter()
            .find(|c| x >= c.left && x < c.right && y >= c.top && y < c.bottom)
            .map(|c| c.hit)
    }
}
pub fn enabled(view: &CandidateView, hit: Hit) -> bool {
    view.visible
        && match hit {
            Hit::Candidate(i) => view.items.get(i).is_some_and(|v| v.enabled),
            Hit::Previous | Hit::PreviousDecorative => view.can_page_previous,
            Hit::Next | Hit::NextDecorative => view.can_page_next,
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
    pub fn press(&mut self, hit: Option<Hit>, view: &CandidateView) {
        self.pressed = hit.filter(|h| enabled(view, *h));
    }
    pub fn motion(&mut self, hit: Option<Hit>) {
        self.hovered = hit;
        if self.pressed != hit {
            self.pressed = None;
        }
    }
    pub fn release(&mut self, hit: Option<Hit>, view: &CandidateView) -> Option<UiAction> {
        let pressed = self.pressed.take()?;
        if hit != Some(pressed) || !enabled(view, pressed) {
            return None;
        }
        Some(match pressed {
            Hit::Candidate(i) => UiAction::ItemInvoked(i as u32),
            Hit::Previous | Hit::PreviousDecorative => UiAction::NavigatePrevious,
            Hit::Next | Hit::NextDecorative => UiAction::NavigateNext,
        })
    }
}

pub fn pixels(dip: f32, dpi: u32) -> i32 {
    (dip * dpi.max(1) as f32 / 96.0).round().max(1.0) as i32
}
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn both_button_pairs_page_but_do_not_share_press_identity() {
        let layout = Layout::new(9, Role::Candidates);
        let view = CandidateView {
            visible: true,
            can_page_previous: true,
            can_page_next: true,
            ..Default::default()
        };
        for (x, hit, action) in [
            (5.0, Hit::PreviousDecorative, UiAction::NavigatePrevious),
            (19.0, Hit::NextDecorative, UiAction::NavigateNext),
            (96.0, Hit::Previous, UiAction::NavigatePrevious),
            (110.0, Hit::Next, UiAction::NavigateNext),
        ] {
            assert_eq!(layout.hit(x, 152.0), Some(hit));
            let mut gesture = Gesture::default();
            gesture.press(Some(hit), &view);
            assert_eq!(gesture.release(Some(hit), &view), Some(action));
            assert!(!enabled(
                &CandidateView {
                    visible: true,
                    ..Default::default()
                },
                hit
            ));
        }
        assert_eq!(layout.hit(60.0, 152.0), None);
        let mut gesture = Gesture::default();
        gesture.press(Some(Hit::PreviousDecorative), &view);
        assert_eq!(gesture.release(Some(Hit::Previous), &view), None);
    }
    #[test]
    fn independent_window_metrics_and_candidate_rows() {
        let input = Layout::new(9, Role::Input);
        let candidates = Layout::new(9, Role::Candidates);
        assert_eq!((input.width, input.height), (173.0, 26.0));
        assert_eq!((candidates.width, candidates.height), (127.0, 169.0));
        assert!(input.cells.is_empty());
        assert_eq!(candidates.hit(5.0, 5.0), Some(Hit::Candidate(0)));
        assert_eq!(candidates.hit(5.0, 21.0), Some(Hit::Candidate(1)));
        assert_eq!(candidates.hit(96.0, 152.0), Some(Hit::Previous));
    }
    #[test]
    fn page_size_is_not_silently_truncated() {
        let layout = Layout::new(10, Role::Candidates);
        assert_eq!(layout.hit(5.0, 149.0), Some(Hit::Candidate(9)));
        assert_eq!(layout.height, 185.0);
    }
    #[test]
    fn candidate_flips_left_and_clamps_on_negative_monitor() {
        let work = crate::theme_api::Anchor {
            left: -1920,
            top: 0,
            right: 0,
            bottom: 1080,
            valid: true,
        };
        assert_eq!(
            candidate_position((-1800, 100), 173, (127, 169), 8, &work),
            (-1619, 100)
        );
        assert_eq!(
            candidate_position((-200, 1000), 173, (127, 169), 8, &work),
            (-335, 911)
        );
    }
    #[test]
    fn release_outside_cancels() {
        let view = CandidateView {
            visible: true,
            can_page_next: true,
            ..Default::default()
        };
        let mut gesture = Gesture::default();
        gesture.press(Some(Hit::Next), &view);
        gesture.motion(None);
        assert_eq!(gesture.release(Some(Hit::Next), &view), None);
        gesture.press(Some(Hit::Next), &view);
        assert_eq!(
            gesture.release(Some(Hit::Next), &view),
            Some(UiAction::NavigateNext)
        );
    }
}
