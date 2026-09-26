//! WASM 主题 ABI 契约：导入与导出名称、数值常量及宿主和 guest 共享的值类型。
//!
//! 坐标系单位为 DIP（host 负责物理像素换算）；颜色为 0xAARRGGBB；
//! 主题模块必须导出名为 `memory` 的线性内存。
//! 字符串指针均指向 guest 线性内存，具体编码、长度和返回值约定由对应导入定义。

/// 所有 host 函数的导入命名空间。
pub const IMPORT_MODULE: &str = "weasel_v2";

/// 主题模块必须导出的线性内存名称。
pub const MEMORY: &str = "memory";

/// ABI 版本值；生命周期契约及使用示例见 `sdk-rust/src/lib.rs` 与 `sdk-as/assembly/index.ts`。
pub use crate::abi::ABI_VERSION;
use crate::abi::{Action, ErrorCode, Mode, PointerPhase};
/// wasm32 导出：返回 ABI 版本；与能力值分开查询，不打包宿主地址。
pub const EXPORT_ABI_VERSION: &str = "theme_abi_version";
/// wasm32 导出：返回 guest 支持的能力位集合。
pub const EXPORT_CAPABILITIES: &str = "theme_capabilities";
/// wasm32 导出：创建主题实例。
pub const EXPORT_INIT: &str = "theme_create";

// ── host 导入（wasm → host 调用）────────────────────────────────────
/// `fill_rect(x, y, w, h, color)`，追加填充矩形命令。
pub const IMPORT_FILL_RECT: &str = "fill_rect";
/// `fill_rounded_rect(x, y, w, h, radius, color)`，追加填充圆角矩形命令。
pub const IMPORT_FILL_ROUNDED_RECT: &str = "fill_rounded_rect";
/// `stroke_rect(x, y, w, h, color, width)`，追加描边矩形命令。
pub const IMPORT_STROKE_RECT: &str = "stroke_rect";
/// `set_size(w, h)`，窗口内容尺寸（DIP）。
pub const IMPORT_SET_SIZE: &str = "set_size";
/// `set_panel(radius, shadow_radius, offset_x, offset_y, color)`.
/// 参数必须为有限 DIP 值：圆角半径 0..=4096、阴影半径 0..=250、偏移量 -1024..=1024。
/// 阴影上限是宿主的保守资源限制；微软的 `DropShadow.BlurRadius` 文档未规定最大值。
pub const IMPORT_SET_PANEL: &str = "set_panel";
/// `set_backdrop(...)`，设置宿主绘制的持久背景材质及其不透明回退颜色。
pub const IMPORT_SET_BACKDROP: &str = "set_backdrop";
/// `set_visible(0|1)`，控制常驻 guest 的窗口可见性，不销毁 guest 状态。
pub const IMPORT_SET_VISIBLE: &str = "set_visible";
/// `set_fixed_position(x, y)`，设置相对主屏幕工作区原点的 DIP 偏移。
pub const IMPORT_SET_FIXED_POSITION: &str = "set_fixed_position";
/// `begin_drag()`，请求宿主从当前按下位置开始移动固定定位的窗口。
pub const IMPORT_BEGIN_DRAG: &str = "begin_drag";

/// 持续显示的宿主背景材质参数。
///
/// 混合权重分别控制原始模糊背景、着色余辉和纯色分支；GPU 资源由宿主持有，材质不可用时
/// 宿主使用不透明回退色。各系数的范围和启用状态校验由宿主执行。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct BackdropStyle {
    /// 是否启用原生背景材质。
    pub enabled: bool,
    /// 材质色调，格式为 `0xAARRGGBB`。
    pub tint: u32,
    /// 高斯模糊的标准差，单位为 DIP。
    pub blur_sigma: f32,
    /// 原始模糊背景分支的合成系数。
    pub backdrop_balance: f32,
    /// 着色灰度模糊分支的合成系数。
    pub afterglow_balance: f32,
    /// 纯色分支的合成系数。
    pub color_balance: f32,
    /// 材质不可用或关闭时使用的回退色，格式为 `0xAARRGGBB`。
    pub fallback_color: u32,
}
/// `send_action(action, index)`，请求高层 UiAction。
pub const IMPORT_SEND_ACTION: &str = "send_action";
/// `request_frame()`，请求下一动画帧。
pub const IMPORT_REQUEST_FRAME: &str = "request_frame";
/// `time_ms() -> f64`，宿主单调时钟毫秒。
pub const IMPORT_TIME_MS: &str = "time_ms";
/// `log(level, ptr, len)`，写入普通日志；面向用户的通知使用 `report_notice`。
pub const IMPORT_LOG: &str = "log";

/// AssemblyScript 运行时断言/中止导入所属的模块名。
pub const IMPORT_ABORT_MODULE: &str = "env";
/// AssemblyScript 运行时中止导入名：`abort(message, fileName, line, column)`；
/// 前两个参数是 AssemblyScript UTF-16 字符串对象指针。
pub const IMPORT_ABORT: &str = "abort";

// ── 动作 id（对应 theme_api::UiAction）──────────────────────────────
/// 选中第 index 个候选项。
pub const ACTION_ITEM: i32 = Action::Item as i32;
/// 上一页。
pub const ACTION_PREVIOUS: i32 = Action::Previous as i32;
/// 下一页。
pub const ACTION_NEXT: i32 = Action::Next as i32;
/// 打开表情面板。
pub const ACTION_EMOJI: i32 = Action::Emoji as i32;
/// 关闭当前合成，但不激活渲染器窗口。
pub const ACTION_DISMISS: i32 = Action::Dismiss as i32;

// ── 鼠标类型 ─────────────────────────────────────────────────────────
/// 指针按下阶段。
pub const MOUSE_DOWN: i32 = PointerPhase::Down as i32;
/// 指针移动阶段。
pub const MOUSE_MOVE: i32 = PointerPhase::Move as i32;
/// 指针抬起阶段。
pub const MOUSE_UP: i32 = PointerPhase::Up as i32;
/// 指针离开接收区域阶段。
pub const MOUSE_LEAVE: i32 = PointerPhase::Leave as i32;
/// 指针操作被取消阶段。
pub const MOUSE_CANCEL: i32 = PointerPhase::Cancel as i32;

// ── init 的 mode 参数 ────────────────────────────────────────────────
/// 正常运行模式。
pub const MODE_LIVE: i32 = Mode::Live as i32;
/// 预览模式。
pub const MODE_PREVIEW: i32 = Mode::Preview as i32;

// ── init/render 返回的错误码 ─────────────────────────────────────────
/// 操作成功。
pub const ERR_OK: i32 = ErrorCode::Success as i32;
/// 当前视图不可用。
pub const ERR_BAD_VIEW: i32 = ErrorCode::InvalidArgument as i32;
/// 主题内部错误。
pub const ERR_INTERNAL: i32 = ErrorCode::Internal as i32;

/// 单次从 guest 内存读取的字符串上限；各导入另有更小的限额。
pub const MAX_STRING_BYTES: usize = 65_536;

/// 原生圆角面板呈现参数，与 guest 绘制命令分别处理。
///
/// 坐标单位为 DIP，颜色格式为 `0xAARRGGBB`；阴影半径为零时关闭阴影。本类型描述原生
/// 阴影及表面偏移，背景内容仍由 guest 绘制。增加呈现能力需要显式扩展 ABI。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PanelStyle {
    /// 可选的面板范围；`None` 表示整个内容表面，显式范围不包含装饰区域。
    pub bounds: Option<Rect>,
    /// 面板圆角半径，单位为 DIP。
    pub corner_radius: f32,
    /// 阴影模糊半径，单位为 DIP；零值关闭阴影。
    pub shadow_radius: f32,
    /// 相对内容表面的水平偏移，单位为 DIP。
    pub offset_x: f32,
    /// 相对内容表面的垂直偏移，单位为 DIP。
    pub offset_y: f32,
    /// 面板及阴影颜色，格式为 `0xAARRGGBB`。
    pub color: u32,
}

/// 以 DIP 表示的矩形范围。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Rect {
    /// 左边缘坐标。
    pub x: f32,
    /// 上边缘坐标。
    pub y: f32,
    /// 宽度，必须大于零。
    pub w: f32,
    /// 高度，必须大于零。
    pub h: f32,
}
impl Rect {
    /// 判断矩形是否有限、尺寸为正且完全位于给定内容尺寸内。
    ///
    /// 内容尺寸按 `(宽, 高)` 传入；任何坐标或尺寸非有限、越界或非正的矩形均返回
    /// `false`。
    pub fn within(self, size: (f32, f32)) -> bool {
        [self.x, self.y, self.w, self.h]
            .iter()
            .all(|v| v.is_finite())
            && self.x >= 0.0
            && self.y >= 0.0
            && self.w > 0.0
            && self.h > 0.0
            && self.x + self.w <= size.0
            && self.y + self.h <= size.1
    }
}

/// guest 选择的原生窗口定位策略。
///
/// 锚定模式用于常规候选窗；固定模式以主屏幕工作区为参照，位置单位为 DIP。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub enum PlacementStyle {
    /// 使用宿主的常规锚定定位。
    #[default]
    Anchored,
    /// 相对主屏幕工作区原点的固定偏移。
    Fixed {
        /// 水平偏移，单位为 DIP。
        x: f32,
        /// 垂直偏移，单位为 DIP。
        y: f32,
    },
}

/// 单条绘制命令；坐标单位为 DIP，颜色格式为 `0xAARRGGBB`。
///
/// 变换和裁剪通过压栈/出栈影响后续命令；资源类命令持有资源强引用，使资源句柄释放后
/// 已提交帧仍可安全回放。浮点参数不符合画布回放约束的命令会被跳过。
#[derive(Debug, Clone, PartialEq)]
pub enum DrawCommand {
    /// 压入二维仿射变换矩阵 `[m11, m12, m21, m22, dx, dy]`。
    PushTransform([f32; 6]),
    /// 压入矩形裁剪范围。
    PushClip(Rect),
    /// 弹出最近压入的变换或裁剪状态。
    PopState,
    /// 绘制预先排版的文本。
    Layout {
        /// 帧对布局资源的强引用。
        resource: std::sync::Arc<crate::resources::Resource>,
        /// 文本左上角横坐标。
        x: f32,
        /// 文本左上角纵坐标。
        y: f32,
        /// 文本颜色。
        color: u32,
        /// 光晕强度与颜色；强度由资源导入约束在 0..=4。
        glow: (f32, u32),
    },
    /// 绘制解码后的图像资源。
    Image {
        /// 帧对图像资源的强引用。
        resource: std::sync::Arc<crate::resources::Resource>,
        /// 目标矩形左上角横坐标。
        x: f32,
        /// 目标矩形左上角纵坐标。
        y: f32,
        /// 目标宽度，必须为正。
        w: f32,
        /// 目标高度，必须为正。
        h: f32,
        /// 不透明度，范围为 0..=1。
        opacity: f32,
    },
    /// 绘制填充圆角矩形。
    FillRoundedRect {
        /// 左边缘坐标。
        x: f32,
        /// 上边缘坐标。
        y: f32,
        /// 矩形宽度。
        w: f32,
        /// 矩形高度。
        h: f32,
        /// 圆角半径。
        radius: f32,
        /// 填充颜色。
        color: u32,
    },
    /// 绘制填充矩形。
    FillRect {
        /// 左边缘坐标。
        x: f32,
        /// 上边缘坐标。
        y: f32,
        /// 矩形宽度。
        w: f32,
        /// 矩形高度。
        h: f32,
        /// 填充颜色。
        color: u32,
    },
    /// 绘制矩形边框。
    StrokeRect {
        /// 左边缘坐标。
        x: f32,
        /// 上边缘坐标。
        y: f32,
        /// 矩形宽度。
        w: f32,
        /// 矩形高度。
        h: f32,
        /// 描边颜色。
        color: u32,
        /// 描边宽度。
        width: f32,
    },
}

impl DrawCommand {
    /// 判断命令的浮点参数是否满足回放所需的有限性约束。
    ///
    /// 变换矩阵分量绝对值还必须不大于 65536，裁剪尺寸必须为正；其他命令只检查参数有限。
    /// 不满足条件的命令由画布回放阶段跳过，因为 D2D 对非有限坐标的行为未定义。
    pub fn is_finite(&self) -> bool {
        match self {
            Self::PushTransform(m) => m.iter().all(|v| v.is_finite() && v.abs() <= 65536.0),
            Self::PushClip(r) => {
                [r.x, r.y, r.w, r.h].iter().all(|v| v.is_finite()) && r.w > 0.0 && r.h > 0.0
            }
            Self::PopState => true,
            DrawCommand::Layout { x, y, .. } => [x, y].iter().all(|v| v.is_finite()),
            DrawCommand::Image {
                x,
                y,
                w,
                h,
                opacity,
                ..
            } => [x, y, w, h, opacity].iter().all(|v| v.is_finite()),
            DrawCommand::FillRoundedRect {
                x, y, w, h, radius, ..
            } => [x, y, w, h, radius].iter().all(|v| v.is_finite()),
            DrawCommand::FillRect { x, y, w, h, .. } => [x, y, w, h].iter().all(|v| v.is_finite()),
            DrawCommand::StrokeRect {
                x, y, w, h, width, ..
            } => [x, y, w, h, width].iter().all(|v| v.is_finite()),
        }
    }
}
