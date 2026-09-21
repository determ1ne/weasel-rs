// One finite native opacity transition per View event; no WASM frame loop.
import { ABI_VERSION, EventKind, FrameResult, ErrorCode } from "../assembly/lifecycle";
import { readView } from "../assembly/view";
import { draw, fill_rect, set_font } from "../assembly/draw";
import { set_size } from "../assembly/surface";
import { with_layer, layer_remove, layer_set, layer_animate, LayerProperty, Easing } from "../assembly/layers";

let hasLayer: bool = false;
let brightTarget: bool = false;
function removeDecoration(): void {
  if (hasLayer) layer_remove(1);
  hasLayer = false;
}
export function theme_abi_version(): i32 { return ABI_VERSION; }
export function theme_capabilities(): i32 { return 0; }
export function theme_create(_mode: i32, _dark: i32): i32 {
  set_font(0, "Microsoft YaHei UI"); return ErrorCode.Success;
}
export function theme_event(kind: i32, _detail: i32, _x: f32, _y: f32, _now: f64): i32 {
  if (kind == EventKind.Hide) {
    hasLayer = false; return FrameResult.Keep; // Host clears retained layers on Hide.
  }
  if (kind != EventKind.View && kind != EventKind.Appearance) return FrameResult.Keep;
  const view = readView();
  if (view === null || !view.visible || view.items.length == 0) {
    removeDecoration();
    fill_rect(0, 0, 1, 1, 0); // Explicit main replacement, not a layer-only Present.
    return FrameResult.Present;
  }
  set_size(240, 48);
  fill_rect(0, 0, 240, 48, 0xff202020);
  draw(view.items[0].primary, 12, 8, 0, 20, 0xffffffff);
  if (!hasLayer) {
    with_layer(1, 240, 48, (): void => { fill_rect(4, 8, 3, 32, 0xff60c0ff); });
    layer_set(1, LayerProperty.Opacity, 0.25); // 只在创建时设置初值。
    brightTarget = false;
    hasLayer = true;
  }
  if (kind == EventKind.View) {
    brightTarget = !brightTarget;
    // 后续 View 只改变目标，从当前显示值连续转向，不重置起点。
    layer_animate(1, LayerProperty.Opacity, brightTarget ? 1 : 0.25, 450, Easing.SmoothStep);
  }
  // Existing layer content and running animations survive this main-frame Present.
  return FrameResult.Present;
}
