//! 主画面区域随主帧重建，图层区域随图层内容重建。动作仅 Down/Up，拖动仅 Down。
//! 发送的是已展示快照中的页内候选索引；不要在 Cancel/Leave 执行点击。
use crate::raw;
pub use crate::types::{Action, PointerPhase};
/// Begin moving a fixed window from the current pointer-down callback.
pub fn begin_drag() {
    unsafe { raw::begin_drag() }
}
pub fn send_action(action: i32, index: i32) {
    unsafe {
        raw::send_action(action, index);
    }
}
/// 在当前目标内声明局部命中区域；正数 id 在目标内唯一，后声明者优先。
/// 这不是跨进程鼠标穿透承诺；阴影和透明区域仍由 native 窗口系统处理。
pub fn hit_region(id: i32, x: f32, y: f32, w: f32, h: f32, radius: f32) {
    unsafe {
        raw::hit_region(id, x, y, w, h, radius);
    }
}
/// 当前指针事件命中区域；-1 未命中，0 为默认面板。
pub fn pointer_region() -> i32 {
    unsafe { raw::pointer_region() }
}
/// 当前事件的图层ID，0代表主画面或未命中；结合 pointer_region 判断。
/// 宿主按时间线近似命中，可能与最后呈现帧有少量时间差。
pub fn pointer_layer() -> i32 {
    unsafe { raw::pointer_layer() }
}
