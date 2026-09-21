/** Event-only retained decorations. Changes commit on Present, not Keep/error.
 * Above main by z-index then creation order; local hit regions with opt-in interaction. At most eight positive IDs.
 */
import * as raw from "./raw";
import { Easing, LayerProperty, LayerStop } from "./types";
export { Easing, LayerProperty, LayerStop } from "./types";

/** Replace layer content, then return to main. Do not nest scopes. Both the outer
 * and callback clip/transform stacks must be balanced at each switch. Dimensions
 * must be positive; hit regions use untransformed local coordinates. Do not change the surface in draw().
 */
export function with_layer(id: i32, width: f32, height: f32, draw: () => void): void {
  assert(id > 0);
  raw.layer_content(id, width, height);
  draw();
  raw.layer_content(0, 0, 0);
  // A trap aborts the event; the host resets the target for the next event.
}
export function layer_remove(id: i32): void { raw.layer_remove(id); }
/** Default 0, larger in front; ties use creation order. Signed indices only order
 * retained layers above main. Present commits without rebuilding surfaces/animations. */
export function layer_z_index(id: i32, z_index: i32): void { raw.layer_z_index(id, z_index); }
/** Fixed content-coordinate clip, independent of layer transforms. Also clips hits. */
export function layer_clip(id: i32, x: f32, y: f32, width: f32, height: f32): void {
  raw.layer_clip(id, 1, x, y, width, height);
}
export function layer_clear_clip(id: i32): void { raw.layer_clip(id, 0, 0, 0, 0, 0); }
/** Defaults off; independent of opacity. */
export function layer_interactive(id: i32, enabled: bool): void { raw.layer_interactive(id, enabled ? 1 : 0); }
export function layer_set(id: i32, property: LayerProperty, value: f32): void {
  raw.layer_set(id, property, value);
}
/** Host retargets from current value, or snaps when animations are disabled.
 * Finite duration; no WASM animation loop required. Bad arguments trap.
 */
export function layer_animate(id: i32, property: LayerProperty, to: f32, duration_ms: f64, easing: Easing = Easing.Linear): void {
  raw.layer_animate(id, property, to, duration_ms, easing);
}
export function layer_stop(id: i32, property: LayerProperty, behavior: LayerStop = LayerStop.Current): void {
  raw.layer_stop(id, property, behavior);
}
