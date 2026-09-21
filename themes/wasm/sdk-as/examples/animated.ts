// Full redraw pulse: 250 ms transition, then sleep until the next 750 ms boundary.
import { ABI_VERSION, EventKind, FrameResult, ErrorCode } from "../assembly/lifecycle";
import { readView } from "../assembly/view";
import { draw, fill_rect, set_font } from "../assembly/draw";
import { set_size } from "../assembly/surface";
import { hit_region, pointer_region, send_action, Action, PointerPhase } from "../assembly/interaction";
import { Tween, Easing, request_frame, request_wakeup, cancel_wakeup } from "../assembly/animation";

const pulse = new Tween(0);
let epoch: f64 = -1;
let phase: f64 = -1;
function reset(): void { cancel_wakeup(); epoch = -1; phase = -1; pulse.snap(0); }
export function theme_abi_version(): i32 { return ABI_VERSION; }
export function theme_capabilities(): i32 { return 0; }
export function theme_create(_mode: i32, _dark: i32): i32 {
  set_font(0, "Microsoft YaHei UI"); // The drawing helpers cache fonts and text layouts.
  return ErrorCode.Success;
}
function paint(now: f64): i32 {
  const view = readView(); // Always use the latest view, including on Animation.
  if (view === null || !view.visible || view.items.length === 0) {
    reset(); return FrameResult.Present;
  }
  let amount: f64 = 1;
  if (isFinite(now)) {
    if (epoch < 0) epoch = now;
    const nextPhase = Math.floor(Math.max(0, now - epoch) / 750);
    if (nextPhase != phase) {
      phase = nextPhase;
      pulse.retarget(phase % 2 == 0 ? 1 : 0, now, 250, Easing.SmoothStep);
    }
    amount = pulse.value(now);
    if (!pulse.finished(now)) request_frame();
    else request_wakeup(epoch + (phase + 1) * 750);
  } else reset(); // Invalid clock: static indicator, no outstanding wakeups.
  set_size(240, 48);
  fill_rect(0, 0, 240, 48, 0xff202020);
  fill_rect(4, 8, 3, 32, (<u32>(80 + 175 * amount) << 24) | 0x0060c0ff);
  draw(view.items[0].primary, 12, 8, 0, 20, 0xffffffff);
  if (view.items[0].enabled) hit_region(1, 0, 0, 240, 48, 0);
  return FrameResult.Present; // Rebuild all commands AND hit regions every time.
}
export function theme_event(kind: i32, detail: i32, _x: f32, _y: f32, now: f64): i32 {
  switch (kind) {
    case EventKind.View:
    case EventKind.Appearance:
    case EventKind.Animation: return paint(now);
    case EventKind.Hide: reset(); return FrameResult.Keep;
    case EventKind.Pointer:
      if (detail == PointerPhase.Down && pointer_region() == 1) send_action(Action.Item, 0);
      return FrameResult.Keep;
    default: return ErrorCode.InvalidArgument;
  }
}
