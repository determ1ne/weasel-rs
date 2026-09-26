//! 设置主题展示表面的持久属性，包括内容范围、锚点、面板材质和驻留可见状态。
//!
//! 创建回调可初始化表面；事件内的设置在返回 `FrameResult::Present` 时提交，返回 `Keep`、
//! 负错误或 trap 时丢弃。内容坐标、应用光标定位锚点和面板材质区域互相独立；这些设置
//! 不绘制内容，也不受 [`crate::draw`] 的变换或裁剪影响。长度使用 DIP，颜色为 ARGB。
use crate::{raw, types::{ErrorCode, SurfaceKind}};

/// 由宿主管理的原生表面句柄。
///
/// `primary()` 不拥有表面；`create()` 返回的辅助表面会在 `Drop` 时请求销毁。主题仍在
/// 单线程事件回调内使用这些句柄，不能把它们解释成 HWND 或跨主题实例传递。
pub struct Surface {
    id: i32,
    owned: bool,
}

impl Surface {
    /// 返回始终存在的主候选表面。
    pub const fn primary() -> Self {
        Self { id: 0, owned: false }
    }

    /// 创建辅助原生表面。每个主题最多拥有八个表面（包含主表面）。
    pub fn create(kind: SurfaceKind) -> Result<Self, ErrorCode> {
        let id = unsafe { raw::surface_create(kind as i32) };
        if id > 0 {
            Ok(Self { id, owned: true })
        } else {
            Err(ErrorCode::try_from(id).unwrap_or(ErrorCode::Internal))
        }
    }

    /// 返回仅用于 ABI 调用的不透明 ID。
    pub const fn id(&self) -> i32 {
        self.id
    }

    /// 选择此表面作为后续绘制、图层、几何和命中区操作的目标。
    pub fn select(&self) -> Result<(), ErrorCode> {
        let code = unsafe { raw::surface_select(self.id) };
        if code == ErrorCode::Success as i32 {
            Ok(())
        } else {
            Err(ErrorCode::try_from(code).unwrap_or(ErrorCode::Internal))
        }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        if self.owned {
            let _ = unsafe { raw::surface_destroy(self.id) };
        }
    }
}

/// 当前指针事件来源的表面；非指针事件返回主表面 ID 0。
pub fn event_surface() -> i32 {
    unsafe { raw::event_surface() }
}
/// 由宿主合成的背景材质设置。
///
/// 模糊 sigma 以 DIP 表示，三个权重须为非负数且总和为 1；宿主负责背景采样、效果合成，
/// 材质不可用时使用不透明回退颜色。
pub struct BackdropStyle {
    /// 是否启用宿主背景材质。
    pub enabled: bool,
    /// 宿主材质 tint 参数的 ABI 编码值。
    pub tint: u32,
    /// 背景模糊 sigma，单位为 DIP。
    pub blur_sigma: f32,
    /// 原始背景在材质混合中的权重。
    pub backdrop_balance: f32,
    /// 残影效果在材质混合中的权重。
    pub afterglow_balance: f32,
    /// 颜色层在材质混合中的权重。
    pub color_balance: f32,
    /// 材质不可用时绘制的不透明 ARGB 回退色。
    pub fallback_color: u32,
}

/// 设置持久背景材质；事件内调用须由 `Present` 提交。
pub fn set_backdrop(style: &BackdropStyle) {
    unsafe {
        raw::set_backdrop(
            style.enabled as i32,
            style.tint as i32,
            style.blur_sigma,
            style.backdrop_balance,
            style.afterglow_balance,
            style.color_balance,
            style.fallback_color as i32,
        );
    }
}

/// 声明完整内容尺寸和用于相对应用光标定位的锚点矩形。
///
/// `anchor` 为内容坐标中的 `[x, y, width, height]`；内容可以大于锚点。面板范围需另用
/// [`panel_bounds`] 设置，锚点不会裁剪内容。调用后不要再以 [`set_size`] 覆盖此几何。
pub fn frame_geometry(width: f32, height: f32, anchor: [f32; 4]) {
    unsafe {
        raw::frame_geometry(width, height, anchor[0], anchor[1], anchor[2], anchor[3]);
    }
}
/// 设置内容坐标内的面板矩形 `[x, y, width, height]`，仅决定材质、圆角遮罩和阴影范围。
///
/// 面板必须位于内容表面内；它不会裁剪图片或其他绘制内容。
pub fn panel_bounds(rect: [f32; 4]) {
    unsafe {
        raw::panel_bounds(rect[0], rect[1], rect[2], rect[3]);
    }
}
/// 设置普通矩形内容表面的 DIP 尺寸，不包含宿主阴影。
pub fn set_size(w: f32, h: f32) {
    unsafe {
        raw::set_size(w, h);
    }
}
/// 显示或隐藏驻留主题表面，同时保留 WASM 实例状态；失焦隐藏仍由宿主优先控制。
pub fn set_visible(visible: bool) {
    unsafe { raw::set_visible(visible as i32) }
}
/// 将驻留主题固定在主屏工作区的 DIP 偏移位置；原生窗口移动由宿主管理。
pub fn set_fixed_position(x: f32, y: f32) {
    unsafe { raw::set_fixed_position(x, y) }
}
/// 设置原生面板圆角、阴影和阴影颜色。
///
/// 圆角范围为 0–4096 DIP，阴影半径为 0–250 DIP，偏移为 −1024–1024 DIP；参数须有限。
/// 颜色为 ARGB，阴影半径为零时禁用阴影。
pub fn set_panel(radius: f32, shadow_radius: f32, offset_x: f32, offset_y: f32, color: u32) {
    unsafe { raw::set_panel(radius, shadow_radius, offset_x, offset_y, color as i32) }
}
