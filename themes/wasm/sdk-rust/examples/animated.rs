//! Full redraw pulse: 250 ms transition, then sleep until a 750 ms boundary.
use std::cell::RefCell;
use weasel_wasm_sdk::{
    animation::{self, Easing, Tween},
    draw::{draw, fill_rect, set_font},
    interaction::{Action, PointerPhase, hit_region, pointer_region, send_action},
    lifecycle::{ABI_VERSION, ErrorCode, EventKind, FrameResult},
    surface::set_size,
    view::View,
};
struct Pulse {
    tween: Tween,
    epoch: Option<f64>,
    phase: f64,
}
impl Pulse {
    fn new() -> Self {
        Self {
            tween: Tween::new(0.0),
            epoch: None,
            phase: -1.0,
        }
    }
    fn reset(&mut self) {
        animation::cancel_wakeup();
        *self = Self::new();
    }
    fn sample(&mut self, now: f64) -> f64 {
        if !now.is_finite() {
            self.reset();
            return 1.0; // Invalid clock: static, no pending wakeups.
        }
        let epoch = *self.epoch.get_or_insert(now);
        let phase = ((now - epoch).max(0.0) / 750.0).floor();
        if phase != self.phase {
            self.phase = phase;
            self.tween.retarget(
                if phase % 2.0 == 0.0 { 1.0 } else { 0.0 },
                now,
                250.0,
                Easing::SmoothStep,
            );
        }
        let value = self.tween.value(now);
        if self.tween.finished(now) {
            animation::request_wakeup(epoch + (phase + 1.0) * 750.0);
        } else {
            animation::request_frame();
        }
        value
    }
}
thread_local! { static PULSE: RefCell<Pulse> = RefCell::new(Pulse::new()); }
#[unsafe(no_mangle)]
pub extern "C" fn theme_abi_version() -> u32 {
    ABI_VERSION as u32
}
#[unsafe(no_mangle)]
pub extern "C" fn theme_capabilities() -> u32 {
    0
}
#[unsafe(no_mangle)]
pub extern "C" fn theme_create(_mode: i32, _dark: i32) -> i32 {
    set_font(0, "Microsoft YaHei UI"); // Drawing helpers cache fonts and layouts.
    ErrorCode::Success as i32
}
fn paint(now: f64) -> i32 {
    // Read the latest snapshot even for Animation, never capture an old candidate.
    let view = View::read().filter(|v| v.visible && !v.items.is_empty());
    if let Some(view) = view {
        let amount = PULSE.with(|p| p.borrow_mut().sample(now));
        set_size(240.0, 48.0);
        fill_rect(0.0, 0.0, 240.0, 48.0, 0xff202020);
        fill_rect(
            4.0,
            8.0,
            3.0,
            32.0,
            (((80.0 + 175.0 * amount) as u32) << 24) | 0x0060c0ff,
        );
        draw(&view.items[0].primary, 12.0, 8.0, 0, 20.0, 0xffffffff);
        if view.items[0].enabled {
            hit_region(1, 0.0, 0.0, 240.0, 48.0, 0.0);
        }
    } else {
        PULSE.with(|p| p.borrow_mut().reset());
    }
    FrameResult::Present as i32 // All commands and hit regions, or empty to clear.
}
#[unsafe(no_mangle)]
pub extern "C" fn theme_event(kind: i32, detail: i32, _x: f32, _y: f32, now: f64) -> i32 {
    match EventKind::try_from(kind) {
        Ok(EventKind::View | EventKind::Appearance | EventKind::Animation) => return paint(now),
        Ok(EventKind::Hide) => PULSE.with(|p| p.borrow_mut().reset()),
        Ok(EventKind::Pointer) => {
            if detail == PointerPhase::Down as i32 && pointer_region() == 1 {
                send_action(Action::Item as i32, 0);
            }
        }
        Err(_) => return ErrorCode::InvalidArgument as i32,
    }
    FrameResult::Keep as i32
}
