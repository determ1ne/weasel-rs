//! 管理主画面上方可跨事件保留的装饰或交互图层。
//!
//! 所有编辑只能在主题事件中发起，并在返回 `Present` 时提交；`Keep`、错误或 trap 会丢弃
//! 图层编辑。图层按 z-index 和创建顺序绘制，始终位于主画面上方。图层内容重绘会重建该层
//! 命中区域；交互默认关闭，需显式启用。
use crate::raw;
pub use crate::types::{Easing, LayerProperty, LayerStop};

/// 选择并清空指定图层的命令目标，执行闭包后恢复主画面目标。
///
/// ID 必须为正数，同时最多保留 8 个图层；ID 不要求落在 `1..=8`。宽高须为有效正尺寸，
/// 作用域不得嵌套。闭包内可绘制并注册局部命中区域，但不能更改表面属性；绘制裁剪/变换
/// 栈须在闭包返回前平衡。即使闭包 panic，RAII 守卫也会尝试切回主目标。
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
/// 请求移除图层内容及其原生动画；在本事件以 `Present` 提交后生效。
pub fn layer_remove(id: i32) {
    unsafe {
        raw::layer_remove(id);
    }
}
/// 设置图层叠放顺序；默认值为 0，数值越大越靠前，相同值按创建顺序排序。
///
/// 有符号值只调整图层之间的顺序，所有图层始终在主画面上方。重排不重建内容或重启动画，
/// 并随 `Present` 提交。
pub fn layer_z_index(id: i32, z_index: i32) {
    unsafe {
        raw::layer_z_index(id, z_index);
    }
}

/// 设置内容坐标系中的固定矩形裁剪 `[x, y, width, height]`；`None` 清除裁剪。
///
/// 裁剪不随图层位移或缩放，并同时约束绘制和指针命中；矩形尺寸须为正数。
pub fn layer_clip(id: i32, rect: Option<[f32; 4]>) {
    let [x, y, w, h] = rect.unwrap_or([0.0; 4]);
    unsafe {
        raw::layer_clip(id, i32::from(rect.is_some()), x, y, w, h);
    }
}
/// 显式启用或关闭图层命中，默认为关闭；透明度不会自动关闭交互。
///
/// 删除后重建的图层属于新一代，不能接收针对旧图层尚未完成的按下/点击。
pub fn layer_interactive(id: i32, enabled: bool) {
    unsafe {
        raw::layer_interactive(id, i32::from(enabled));
    }
}
/// 立即设置图层属性并替换该属性当前的原生动画。
pub fn layer_set(id: i32, property: LayerProperty, value: f32) {
    unsafe {
        raw::layer_set(id, property as i32, value);
    }
}
/// 启动从当前显示值到目标值的有限时长原生动画，不需要 WASM 帧循环。
///
/// 相同属性的新动画替换旧动画而不排队；时长须有限、非负且不超过 3,600,000 ms，零时长
/// 立即到达目标。非法参数会导致宿主 trap。
pub fn layer_animate(id: i32, property: LayerProperty, to: f32, duration_ms: f64, easing: Easing) {
    unsafe {
        raw::layer_animate(id, property as i32, to, duration_ms, easing as i32);
    }
}
/// 停止指定属性动画；`Current` 保持当前显示值，`End` 立即到达动画目标。
pub fn layer_stop(id: i32, property: LayerProperty, behavior: LayerStop) {
    unsafe {
        raw::layer_stop(id, property as i32, behavior as i32);
    }
}
