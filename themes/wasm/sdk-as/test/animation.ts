import { Tween, Easing, progress, lerp, ease, request_frame, request_wakeup, cancel_wakeup } from "../assembly/animation";
import { with_layer, layer_set, layer_animate, layer_stop, layer_remove, LayerProperty, LayerStop } from "../assembly/layers";

export function math(): void {
  const t = new Tween(0);
  t.retarget(10, 100, 100);
  assert(t.value(50) == 0 && t.value(150) == 5);
  t.retarget(20, 150, 100, Easing.EaseOut);
  assert(t.value(150) == 5 && t.value(200) == 16.25 && t.finished(250));
  t.retarget(7, 250, 0);
  assert(t.value(250) == 7);
  assert(progress(1, 0, -1) == 1 && progress(NaN, 0, 100) == 1);
  assert(progress(1, 0, Infinity) == 1 && progress(1, NaN, 100) == 1);
  assert(lerp(-f64.MAX_VALUE, f64.MAX_VALUE, 0.5) == 0);
  assert(lerp(NaN, Infinity, 0.5) == 0);
  assert(<i32>Easing.Linear == 0 && <i32>Easing.SmoothStep == 1 && <i32>Easing.EaseIn == 2 && <i32>Easing.EaseOut == 3);
  assert(ease(0.5, Easing.SmoothStep) == 0.5 && ease(0.5, Easing.EaseIn) == 0.25 && ease(0.5, Easing.EaseOut) == 0.75);
}
export function schedule(): void {
  request_wakeup(NaN); request_wakeup(Infinity); request_wakeup(-1);
  request_wakeup(100); request_wakeup(200); cancel_wakeup(); request_wakeup(300); request_frame();
}
export function layers(): void {
  with_layer(42, 100, 50, (): void => {});
  layer_set(42, LayerProperty.Opacity, 0.5);
  layer_animate(42, LayerProperty.Opacity, 1, 200, Easing.SmoothStep);
  layer_stop(42, LayerProperty.Opacity, LayerStop.End);
  layer_remove(42);
}
