//! Monotonic millisecond timelines and scalar tweens; scheduling is explicit.
//! See the crate guide for wakeup coalescing and full-frame redraw rules.

use crate::raw as host;
pub use crate::types::Easing;

/// Host monotonic absolute milliseconds, in the same clock domain as event `now`.
pub fn time_ms() -> f64 {
    crate::time_ms()
}
/// Request the next animation event.
pub fn request_frame() {
    crate::request_frame();
}
/// Request an absolute deadline. Pending earliest requests are never postponed.
/// Nonfinite/negative deadlines are ignored.
/// A deadline more than 24 hours ahead traps in the host.
pub fn request_wakeup(deadline_ms: f64) {
    if deadline_ms.is_finite() && deadline_ms >= 0.0 {
        unsafe {
            host::request_wakeup(deadline_ms);
        }
    }
}
/// Clear all outstanding WASM wakeups (including frame requests). Hide also cancels.
pub fn cancel_wakeup() {
    unsafe {
        host::cancel_wakeup();
    }
}

fn finite(value: f64, fallback: f64) -> f64 {
    if value.is_finite() { value } else { fallback }
}
fn unit(value: f64) -> f64 {
    finite(value, 1.0).clamp(0.0, 1.0)
}

/// Clamp progress to [0, 1]. Invalid clocks/durations and duration <= 0 finish.
pub fn progress(now_ms: f64, start_ms: f64, duration_ms: f64) -> f64 {
    if !now_ms.is_finite()
        || !start_ms.is_finite()
        || !duration_ms.is_finite()
        || duration_ms <= 0.0
    {
        return 1.0;
    }
    if now_ms <= start_ms {
        return 0.0;
    }
    unit((now_ms - start_ms) / duration_ms)
}

/// SmoothStep = t*t*(3-2*t), EaseIn = t*t, EaseOut = t*(2-t).
pub fn ease(t: f64, easing: Easing) -> f64 {
    let t = unit(t);
    match easing {
        Easing::Linear => t,
        Easing::SmoothStep => t * t * (3.0 - 2.0 * t),
        Easing::EaseIn => t * t,
        Easing::EaseOut => t * (2.0 - t),
    }
}

/// Clamped interpolation; invalid `from` becomes 0, invalid `to` becomes `from`.
pub fn lerp(from: f64, to: f64, t: f64) -> f64 {
    let from = finite(from, 0.0);
    let to = finite(to, from);
    let t = unit(t);
    if t == 0.0 {
        return from;
    }
    if t == 1.0 {
        return to;
    }
    // Weighted endpoints avoid overflowing the subtraction for opposite signs.
    from * (1.0 - t) + to * t
}

#[derive(Clone, Copy, Debug)]
pub struct Timeline {
    start_ms: f64,
    duration_ms: f64,
}
impl Timeline {
    pub fn new(start_ms: f64, duration_ms: f64) -> Self {
        Self {
            start_ms,
            duration_ms,
        }
    }
    pub fn restart(&mut self, start_ms: f64, duration_ms: f64) {
        *self = Self::new(start_ms, duration_ms);
    }
    pub fn progress(&self, now_ms: f64) -> f64 {
        progress(now_ms, self.start_ms, self.duration_ms)
    }
    pub fn finished(&self, now_ms: f64) -> bool {
        self.progress(now_ms) >= 1.0
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Tween {
    from: f64,
    to: f64,
    timeline: Timeline,
    easing: Easing,
}
impl Tween {
    pub fn new(value: f64) -> Self {
        let value = finite(value, 0.0);
        Self {
            from: value,
            to: value,
            timeline: Timeline::new(0.0, 0.0),
            easing: Easing::Linear,
        }
    }
    pub fn progress(&self, now_ms: f64) -> f64 {
        self.timeline.progress(now_ms)
    }
    pub fn finished(&self, now_ms: f64) -> bool {
        self.timeline.finished(now_ms)
    }
    pub fn value(&self, now_ms: f64) -> f64 {
        lerp(self.from, self.to, ease(self.progress(now_ms), self.easing))
    }
    /// Start from the current sampled value, preserving value continuity.
    pub fn retarget(&mut self, target: f64, now_ms: f64, duration_ms: f64, easing: Easing) {
        self.from = self.value(now_ms);
        self.to = finite(target, self.from);
        self.timeline.restart(now_ms, duration_ms);
        self.easing = easing;
    }
    pub fn snap(&mut self, value: f64) {
        *self = Self::new(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn timeline_and_retarget() {
        let mut t = Tween::new(0.0);
        t.retarget(10.0, 100.0, 100.0, Easing::Linear);
        assert_eq!(t.value(50.0), 0.0);
        assert_eq!(t.value(150.0), 5.0);
        t.retarget(20.0, 150.0, 100.0, Easing::EaseOut);
        assert_eq!(t.value(150.0), 5.0);
        assert_eq!(t.value(200.0), 16.25);
        assert!(t.finished(250.0));
        t.retarget(7.0, 250.0, 0.0, Easing::Linear);
        assert_eq!(t.value(250.0), 7.0);
    }
    #[test]
    fn finite_math_and_easing_contract() {
        for duration in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(progress(1.0, 0.0, duration), 1.0);
        }
        assert_eq!(progress(f64::NAN, 0.0, 100.0), 1.0);
        assert_eq!(lerp(-f64::MAX, f64::MAX, 0.5), 0.0);
        assert_eq!(lerp(f64::NAN, f64::INFINITY, 0.5), 0.0);
        for (e, id, mid) in [
            (Easing::Linear, 0, 0.5),
            (Easing::SmoothStep, 1, 0.5),
            (Easing::EaseIn, 2, 0.25),
            (Easing::EaseOut, 3, 0.75),
        ] {
            assert_eq!(e as i32, id);
            assert_eq!(ease(0.5, e), mid);
            assert_eq!(ease(-1.0, e), 0.0);
            assert_eq!(ease(2.0, e), 1.0);
        }
    }
}
