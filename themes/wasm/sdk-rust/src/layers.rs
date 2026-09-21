//! Retained layers above the main frame, ordered by z-index then creation order.
//! Event-only; edits commit with Present and are discarded with Keep/error.
//! hit_region registers local regions in the selected layer; interaction is opt-in.
use crate::raw;
pub use crate::types::{Easing, LayerProperty, LayerStop};

/// Replace a positive-ID layer's content, then return to the main draw target.
/// At most eight layers may exist; IDs need not be in 1..=8. Do not nest scopes.
/// Width/height must be valid positive dimensions. The closure must balance its
/// clip/transform stack; hit regions use untransformed local coordinates. No surface changes inside it.
pub fn with_layer<R>(id: i32, width: f32, height: f32, draw: impl FnOnce() -> R) -> R {
    assert!(id > 0, "decoration layer IDs must be positive");
    unsafe {
        raw::layer_content(id, width, height);
    }
    struct MainTarget;
    impl Drop for MainTarget {
        fn drop(&mut self) {
            unsafe {
                raw::layer_content(0, 0.0, 0.0);
            }
        }
    }
    let _restore = MainTarget;
    draw()
}
/// Remove retained content and its native animations on Present.
pub fn layer_remove(id: i32) {
    unsafe {
        raw::layer_remove(id);
    }
}
/// Default 0; larger values are in front. Ties use creation order.
/// Signed indices only order retained layers, never behind the main frame.
/// Reordering preserves surfaces and animations; commits on Present.
pub fn layer_z_index(id: i32, z_index: i32) {
    unsafe {
        raw::layer_z_index(id, z_index);
    }
}

/// Fixed clip in content coordinates, independent of this layer's offset/scale.
/// None clears clipping. Also clips pointer hits; positive dimensions required.
pub fn layer_clip(id: i32, rect: Option<[f32; 4]>) {
    let [x, y, w, h] = rect.unwrap_or([0.0; 4]);
    unsafe {
        raw::layer_clip(id, i32::from(rect.is_some()), x, y, w, h);
    }
}
/// Defaults to false; opacity does not disable interaction. Removed/recreated
/// layer generations cannot receive a pending click belonging to the old layer.
pub fn layer_interactive(id: i32, enabled: bool) {
    unsafe {
        raw::layer_interactive(id, i32::from(enabled));
    }
}
/// Set a property immediately, replacing any animation of that property.
pub fn layer_set(id: i32, property: LayerProperty, value: f32) {
    unsafe {
        raw::layer_set(id, property as i32, value);
    }
}
/// Finite native animation from current value; no WASM frame loop is needed.
/// Host validates arguments; zero duration immediately reaches the target.
pub fn layer_animate(id: i32, property: LayerProperty, to: f32, duration_ms: f64, easing: Easing) {
    unsafe {
        raw::layer_animate(id, property as i32, to, duration_ms, easing as i32);
    }
}
pub fn layer_stop(id: i32, property: LayerProperty, behavior: LayerStop) {
    unsafe {
        raw::layer_stop(id, property as i32, behavior as i32);
    }
}
