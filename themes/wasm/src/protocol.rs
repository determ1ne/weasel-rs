//! WASM 主题 ABI 契约：导入/导出名、常量与共享值类型。
//!
//! 坐标系单位为 DIP（host 负责物理像素换算）；颜色为 0xAARRGGBB；
//! 主题模块必须导出名为 `memory` 的线性内存。

/// 所有 host 函数的导入命名空间。
pub const IMPORT_MODULE: &str = "weasel_v2";

/// 主题模块必须导出的线性内存名称。
pub const MEMORY: &str = "memory";

// 生命周期契约及使用示例见 sdk-rust/src/lib.rs 与 sdk-as/assembly/index.ts。
pub use crate::abi::ABI_VERSION;
use crate::abi::{Action, ErrorCode, Mode, PointerPhase};
/// wasm32 ABI：版本和能力分别返回，不打包、不包含宿主地址。
pub const EXPORT_ABI_VERSION: &str = "theme_abi_version";
pub const EXPORT_CAPABILITIES: &str = "theme_capabilities";
pub const EXPORT_INIT: &str = "theme_create";

// ── host 导入（wasm → host 调用）────────────────────────────────────
/// `fill_rect(x, y, w, h, color)`，追加填充矩形命令。
pub const IMPORT_FILL_RECT: &str = "fill_rect";
pub const IMPORT_FILL_ROUNDED_RECT: &str = "fill_rounded_rect";
/// `stroke_rect(x, y, w, h, color, width)`，追加描边矩形命令。
pub const IMPORT_STROKE_RECT: &str = "stroke_rect";
/// `set_size(w, h)`，窗口内容尺寸（DIP）。
pub const IMPORT_SET_SIZE: &str = "set_size";
/// `set_panel(radius, shadow_radius, offset_x, offset_y, color)`.
/// Finite DIP bounds: corner radius 0..=4096, shadow radius 0..=250,
/// offsets -1024..=1024. Shadow bounds are conservative host resource limits;
/// Microsoft's DropShadow.BlurRadius reference does not specify a maximum.
pub const IMPORT_SET_PANEL: &str = "set_panel";
pub const IMPORT_SET_BACKDROP: &str = "set_backdrop";
/// `set_visible(0|1)` lets a resident guest hide without destroying its state.
pub const IMPORT_SET_VISIBLE: &str = "set_visible";
/// `set_fixed_position(x, y)` uses DIP offsets from the primary work area.
pub const IMPORT_SET_FIXED_POSITION: &str = "set_fixed_position";
/// `begin_drag()` asks the host to move a fixed window from this pointer-down.
pub const IMPORT_BEGIN_DRAG: &str = "begin_drag";

/// Persistent background material; weights form a convex RGB blend.
/// The host owns GPU resources and supplies an opaque fallback when unavailable.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct BackdropStyle {
    pub enabled: bool,
    pub tint: u32,
    pub blur_sigma: f32,
    pub backdrop_balance: f32,
    pub afterglow_balance: f32,
    pub color_balance: f32,
    pub fallback_color: u32,
}
/// `send_action(action, index)`，请求高层 UiAction。
pub const IMPORT_SEND_ACTION: &str = "send_action";
/// `request_frame()`，请求下一动画帧。
pub const IMPORT_REQUEST_FRAME: &str = "request_frame";
/// `time_ms() -> f64`，宿主单调时钟毫秒。
pub const IMPORT_TIME_MS: &str = "time_ms";
/// `log(level, ptr, len)`，普通日志；用户通知另走 report_notice。
pub const IMPORT_LOG: &str = "log";

/// AssemblyScript 运行时断言/abort 导入的模块名与函数名：
/// `env.abort(message, fileName, line, column)`，前两项是 AS UTF-16 对象指针。
pub const IMPORT_ABORT_MODULE: &str = "env";
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
/// Dismiss the current composition without activating the renderer window.
pub const ACTION_DISMISS: i32 = Action::Dismiss as i32;

// ── 鼠标类型 ─────────────────────────────────────────────────────────
pub const MOUSE_DOWN: i32 = PointerPhase::Down as i32;
pub const MOUSE_MOVE: i32 = PointerPhase::Move as i32;
pub const MOUSE_UP: i32 = PointerPhase::Up as i32;
pub const MOUSE_LEAVE: i32 = PointerPhase::Leave as i32;
pub const MOUSE_CANCEL: i32 = PointerPhase::Cancel as i32;

// ── init 的 mode 参数 ────────────────────────────────────────────────
pub const MODE_LIVE: i32 = Mode::Live as i32;
pub const MODE_PREVIEW: i32 = Mode::Preview as i32;

// ── init/render 返回的错误码 ─────────────────────────────────────────
pub const ERR_OK: i32 = ErrorCode::Success as i32;
/// 当前视图不可用。
pub const ERR_BAD_VIEW: i32 = ErrorCode::InvalidArgument as i32;
/// 主题内部错误。
pub const ERR_INTERNAL: i32 = ErrorCode::Internal as i32;

/// 单次从 guest 内存读取的字符串上限；各导入另有更小的限额。
pub const MAX_STRING_BYTES: usize = 65_536;

/// Native rounded-panel presentation, independent of guest drawing commands.
/// Coordinates are DIP and color is 0xAARRGGBB. Zero shadow radius disables shadow.
/// This stage describes native shadow and surface offset only; guest drawing owns
/// the background. Future presentation capabilities require an explicit ABI extension.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PanelStyle {
    /// None means the entire content surface; explicit bounds exclude decorations.
    pub bounds: Option<Rect>,
    pub corner_radius: f32,
    pub shadow_radius: f32,
    pub offset_x: f32,
    pub offset_y: f32,
    pub color: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}
impl Rect {
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

/// Native placement policy selected by a guest. Anchored is the normal
/// candidate-window behavior; Fixed is relative to the primary work area.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub enum PlacementStyle {
    #[default]
    Anchored,
    Fixed {
        x: f32,
        y: f32,
    },
}

/// 单条绘制命令；坐标为 DIP，颜色为 0xAARRGGBB。
#[derive(Debug, Clone, PartialEq)]
pub enum DrawCommand {
    PushTransform([f32; 6]),
    PushClip(Rect),
    PopState,
    Layout {
        resource: std::sync::Arc<crate::resources::Resource>,
        x: f32,
        y: f32,
        color: u32,
        glow: (f32, u32),
    },
    Image {
        resource: std::sync::Arc<crate::resources::Resource>,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        opacity: f32,
    },
    FillRoundedRect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radius: f32,
        color: u32,
    },
    FillRect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        color: u32,
    },
    StrokeRect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        color: u32,
        width: f32,
    },
}

impl DrawCommand {
    /// 所有浮点参数是否有限：NaN/Inf 命令会被 canvas 回放时跳过
    /// （D2D 对非有限坐标的行为未定义）。
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
