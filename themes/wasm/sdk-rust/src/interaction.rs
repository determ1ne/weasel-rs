//! 注册内容区域的指针命中，并将指针事件转换为宿主语义动作。
//!
//! 主画面的命中区域随主帧重建，图层区域随图层内容重建；坐标均为各自目标的局部 DIP，
//! 不受绘制变换影响。动作索引绑定当前已展示的候选快照，仅能在 Pointer Down/Up 发送；
//! 固定窗口拖动仅能在 Pointer Down 开始。Cancel/Leave 用于清除主题临时交互状态，不应点击。
use crate::raw;
pub use crate::types::{Action, PointerPhase};
/// 在当前指针按下事件中请求宿主开始拖动固定窗口。
pub fn begin_drag() {
    unsafe { raw::begin_drag() }
}
/// 请求宿主执行语义动作。
///
/// 仅在 Pointer Down/Up 事件中有效；`index` 是已展示快照中的页内候选索引，必须与该
/// 快照对应，不能直接使用新读取但尚未展示的视图索引。
pub fn send_action(action: i32, index: i32) {
    unsafe {
        raw::send_action(action, index);
    }
}
/// 在当前绘制目标内声明圆角命中矩形；图层目标使用图层局部坐标，主目标使用内容坐标。
///
/// 区域 ID 应为正数且在当前目标内唯一；同一目标中后声明的区域优先，主画面和图层合计
/// 最多 256 个。绘制变换/裁剪栈不改变命中几何。命中区域不承诺跨进程鼠标穿透，阴影与
/// 透明像素仍受原生窗口系统约束。
pub fn hit_region(id: i32, x: f32, y: f32, w: f32, h: f32, radius: f32) {
    unsafe {
        raw::hit_region(id, x, y, w, h, radius);
    }
}
/// 返回当前指针事件命中的区域 ID：`-1` 表示未命中，`0` 表示默认面板。
pub fn pointer_region() -> i32 {
    unsafe { raw::pointer_region() }
}
/// 返回当前鼠标事件命中的图层 ID；`0` 表示主画面或未命中，需结合 [`pointer_region`] 判断。
///
/// 宿主按单调时钟估算原生动画中的命中位置，可能与最近显示的 GPU 帧有少量时间差。
pub fn pointer_layer() -> i32 {
    unsafe { raw::pointer_layer() }
}
