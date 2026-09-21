/** Monotonic absolute milliseconds; timeline math never schedules implicitly. */
import { request_frame as hostFrame, time_ms as hostTime,
  request_wakeup as hostWakeup, cancel_wakeup as hostCancel } from "./raw";
import { Easing } from "./types";
export { Easing } from "./types";

export function time_ms(): f64 { return hostTime(); }
export function request_frame(): void { hostFrame(); }
/** Absolute deadline; host keeps the earliest pending request, never postpones it.
 * Nonfinite/negative values are ignored; more than 24 hours ahead traps in host.
 */
export function request_wakeup(deadline_ms: f64): void {
  if (isFinite(deadline_ms) && deadline_ms >= 0) hostWakeup(deadline_ms);
}
/** Clears all outstanding WASM wakeups, including frame requests. Hide also cancels. */
export function cancel_wakeup(): void { hostCancel(); }

function finite(value: f64, fallback: f64): f64 { return isFinite(value) ? value : fallback; }
function unit(value: f64): f64 { return Math.max(0, Math.min(1, finite(value, 1))); }
/** Invalid clocks/durations or duration <= 0 finish immediately. */
export function progress(now_ms: f64, start_ms: f64, duration_ms: f64): f64 {
  if (!isFinite(now_ms) || !isFinite(start_ms) || !isFinite(duration_ms) || duration_ms <= 0) return 1;
  if (now_ms <= start_ms) return 0;
  return unit((now_ms - start_ms) / duration_ms);
}
export function ease(t: f64, easing: Easing = Easing.Linear): f64 {
  t = unit(t);
  switch (easing) {
    case Easing.SmoothStep: return t * t * (3 - 2 * t);
    case Easing.EaseIn: return t * t;
    case Easing.EaseOut: return t * (2 - t);
    default: return t;
  }
}
/** Clamped interpolation; invalid from becomes 0, invalid to becomes from. */
export function lerp(from: f64, to: f64, t: f64): f64 {
  from = finite(from, 0); to = finite(to, from); t = unit(t);
  if (t == 0) return from;
  if (t == 1) return to;
  return from * (1 - t) + to * t;
}
export class Timeline {
  constructor(private start_ms: f64 = 0, private duration_ms: f64 = 0) {}
  restart(start_ms: f64, duration_ms: f64): void {
    this.start_ms = start_ms; this.duration_ms = duration_ms;
  }
  progress(now_ms: f64): f64 { return progress(now_ms, this.start_ms, this.duration_ms); }
  finished(now_ms: f64): bool { return this.progress(now_ms) >= 1; }
}
export class Tween {
  private from: f64;
  private to: f64;
  private timeline: Timeline = new Timeline();
  private easing: Easing = Easing.Linear;
  constructor(value: f64 = 0) { this.from = this.to = finite(value, 0); }
  progress(now_ms: f64): f64 { return this.timeline.progress(now_ms); }
  finished(now_ms: f64): bool { return this.timeline.finished(now_ms); }
  value(now_ms: f64): f64 { return lerp(this.from, this.to, ease(this.progress(now_ms), this.easing)); }
  /** Retarget from the currently sampled value, preserving value continuity. */
  retarget(target: f64, now_ms: f64, duration_ms: f64, easing: Easing = Easing.Linear): void {
    this.from = this.value(now_ms); this.to = finite(target, this.from);
    this.timeline.restart(now_ms, duration_ms); this.easing = easing;
  }
  snap(value: f64): void {
    this.from = this.to = finite(value, 0); this.timeline.restart(0, 0);
  }
}
