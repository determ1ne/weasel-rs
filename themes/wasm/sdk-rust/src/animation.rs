//! 基于宿主单调时钟的时间线、标量补间和事件调度。
//!
//! `time_ms` 与每次事件提供的 `now` 属于同一绝对毫秒时钟域。纯数学工具不会自动
//! 请求事件；动画采样后，主题应自行请求下一帧或绝对唤醒时间，并在需要更新主画面时
//! 重建完整绘制内容。需要仅移动装饰时，可改用 [`crate::layers`] 的原生图层动画。

use crate::raw as host;
pub use crate::types::Easing;

/// 返回宿主单调时钟的绝对毫秒值，与当前事件的 `now` 使用同一时钟域。
pub fn time_ms() -> f64 {
    crate::time_ms()
}
/// 请求一次后续动画事件；每次需要继续动画时都应再次请求。
pub fn request_frame() {
    crate::request_frame();
}
/// 请求在单调时钟的绝对毫秒截止时间后收到唤醒事件。
///
/// 多个待处理请求合并为最早的时间，较晚请求不会推迟它；非有限或负值会被 SDK 忽略。
/// 超前当前时间超过 24 小时会由宿主 trap。该 API 不把截止时间解释为相对延时。
pub fn request_wakeup(deadline_ms: f64) {
    if deadline_ms.is_finite() && deadline_ms >= 0.0 {
        unsafe {
            host::request_wakeup(deadline_ms);
        }
    }
}
/// 取消全部待处理的 WASM 唤醒，包括帧请求；宿主在 Hide 事件时也会自动取消。
///
/// 这不会停止独立运行的原生图层动画。
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

/// 计算并限制时间线进度到 `[0, 1]`。
///
/// 时刻或时长非有限、时长小于等于零时立即返回 `1`；开始时刻之前返回 `0`。
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

/// 将输入限制到 `[0, 1]` 后应用指定曲线：SmoothStep 为 `t²(3−2t)`，EaseIn 为 `t²`，
/// EaseOut 为 `t(2−t)`。
pub fn ease(t: f64, easing: Easing) -> f64 {
    let t = unit(t);
    match easing {
        Easing::Linear => t,
        Easing::SmoothStep => t * t * (3.0 - 2.0 * t),
        Easing::EaseIn => t * t,
        Easing::EaseOut => t * (2.0 - t),
    }
}

/// 以限制到 `[0, 1]` 的进度在端点间插值。
///
/// 非有限 `from` 按 `0` 处理，非有限 `to` 按归一化后的 `from` 处理；加权端点计算
/// 可避免异号大数直接相减时溢出。
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
/// 由绝对开始时刻和持续时间定义的可复制时间线，不保存时钟或宿主句柄。
pub struct Timeline {
    /// 单调时钟域中的绝对开始毫秒。
    start_ms: f64,
    /// 动画持续毫秒；非有限或非正值会视为已完成。
    duration_ms: f64,
}
impl Timeline {
    /// 创建时间线；时刻和时长在采样时按 [`progress`] 的规则处理。
    pub fn new(start_ms: f64, duration_ms: f64) -> Self {
        Self {
            start_ms,
            duration_ms,
        }
    }
    /// 用新的绝对开始时刻和持续时间替换当前时间线。
    pub fn restart(&mut self, start_ms: f64, duration_ms: f64) {
        *self = Self::new(start_ms, duration_ms);
    }
    /// 在给定宿主时刻采样限制后的进度。
    pub fn progress(&self, now_ms: f64) -> f64 {
        progress(now_ms, self.start_ms, self.duration_ms)
    }
    /// 判断给定时刻是否已完成；无效时钟或时长也视为完成。
    pub fn finished(&self, now_ms: f64) -> bool {
        self.progress(now_ms) >= 1.0
    }
}

#[derive(Clone, Copy, Debug)]
/// 在两个标量值间随时间变化的补间；值按需采样，不会自行调度事件。
pub struct Tween {
    from: f64,
    to: f64,
    timeline: Timeline,
    easing: Easing,
}
impl Tween {
    /// 创建静止在初始值的补间；非有限初始值归零。
    pub fn new(value: f64) -> Self {
        let value = finite(value, 0.0);
        Self {
            from: value,
            to: value,
            timeline: Timeline::new(0.0, 0.0),
            easing: Easing::Linear,
        }
    }
    /// 返回当前时间线的限制后进度。
    pub fn progress(&self, now_ms: f64) -> f64 {
        self.timeline.progress(now_ms)
    }
    /// 判断给定时刻是否已完成；无效时钟或时长也视为完成。
    pub fn finished(&self, now_ms: f64) -> bool {
        self.timeline.finished(now_ms)
    }
    /// 返回给定时刻按缓动曲线采样的值。
    pub fn value(&self, now_ms: f64) -> f64 {
        lerp(self.from, self.to, ease(self.progress(now_ms), self.easing))
    }
    /// 从 `now_ms` 的当前采样值开始转向新目标，保持数值连续。
    ///
    /// 速度不保证连续。通常只在目标变化时调用；每帧调用会不断重启时间线。
    pub fn retarget(&mut self, target: f64, now_ms: f64, duration_ms: f64, easing: Easing) {
        self.from = self.value(now_ms);
        self.to = finite(target, self.from);
        self.timeline.restart(now_ms, duration_ms);
        self.easing = easing;
    }
    /// 立即将补间收敛到指定值；非有限值按零处理。
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
