import {
  ABI_VERSION,
  Action,
  Capability,
  ErrorCode,
  EventKind,
  FrameResult,
  PointerPhase,
  View,
  draw_text,
  fill_rect,
  measure_text,
  options,
  readView,
  send_action,
  set_font,
  set_size,
} from "../assembly";

let font: i32 = -1;
let view: View | null = null;
let pressed: i32 = -1;

export function theme_abi_version(): i32 { return ABI_VERSION; }
export function theme_capabilities(): i32 { return Capability.None; }

export function theme_create(_mode: i32, _dark: i32): i32 {
  font = set_font(20, "Segoe UI", 400);
  return ErrorCode.Success;
}

function render(): i32 {
  const next = readView();
  if (next == null || next.items.length == 0) return ErrorCode.InvalidArgument;
  view = next;
  const size = <f32>options.number("/fontSize", 14);
  set_size(Mathf.max(120, measure_text(font, next.items[0].primary, size) + 20), 42);
  fill_rect(0, 0, 120, 42, 0xfff5f5f5);
  draw_text(font, next.items[0].primary, 10, 10, size, 0xff202020);
  return FrameResult.Present;
}

function pointer(phase: i32, x: f32, y: f32): i32 {
  const row = view != null && x >= 0 && y >= 0 && y < 42 ? 0 : -1;
  if (phase == PointerPhase.Down) pressed = row;
  if (phase == PointerPhase.Leave || phase == PointerPhase.Cancel) pressed = -1;
  if (phase == PointerPhase.Up) {
    const target = pressed;
    pressed = -1;
    if (target >= 0 && target == row) send_action(Action.Item, target);
  }
  return FrameResult.Keep;
}

export function theme_event(kind: i32, detail: i32, x: f32, y: f32, _now: f64): i32 {
  if (kind == EventKind.View) return render();
  if (kind == EventKind.Pointer) return pointer(detail, x, y);
  if (kind == EventKind.Hide) { view = null; pressed = -1; }
  return FrameResult.Keep;
}
