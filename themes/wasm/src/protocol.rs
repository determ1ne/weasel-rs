//! WASM 主题 ABI 契约：导入/导出名、常量与共享值类型。
//!
//! 坐标系单位为 DIP（host 负责物理像素换算）；颜色为 0xAARRGGBB；
//! 主题模块必须导出名为 `memory` 的线性内存。

/// 所有 host 函数的导入命名空间。
pub const IMPORT_MODULE: &str = "weasel";

/// 主题模块必须导出的线性内存名称。
pub const MEMORY: &str = "memory";

// ── 主题导出（host → wasm 调用）──────────────────────────────────────
/// `init(mode, dark) -> code`，mode 见 `MODE_*`。
pub const EXPORT_INIT: &str = "init";
/// Native theme ABI, independent of TIP/RPC versions. No legacy JSON ABI support.
pub const ABI_VERSION: i32 = 1;
pub const EXPORT_ABI_VERSION: &str = "abi_version";
/// `render() -> code`；通过 data_* 导入读取宿主快照。
pub const EXPORT_RENDER: &str = "render";
/// `mouse(kind, x, y)`，窗口局部 DIP 坐标。
pub const EXPORT_MOUSE: &str = "mouse";
/// `frame(now_ms)`，动画帧回调。
pub const EXPORT_FRAME: &str = "frame";
/// `hide()`。
pub const EXPORT_HIDE: &str = "hide";
/// `refresh(dark)`，外观变化。
pub const EXPORT_REFRESH: &str = "refresh";

// ── host 导入（wasm → host 调用）────────────────────────────────────
/// `measure_text(ptr, len, font, size) -> f32`，返回文本宽度（DIP）。
/// Additional imports: set_font(slot, UTF-8 ptr, len); line_height(slot, size) -> DIP height.
pub const IMPORT_MEASURE_TEXT: &str = "measure_text";
/// `fill_rect(x, y, w, h, color)`，追加填充矩形命令。
pub const IMPORT_FILL_RECT: &str = "fill_rect";
pub const IMPORT_FILL_ROUNDED_RECT: &str = "fill_rounded_rect";
/// `stroke_rect(x, y, w, h, color, width)`，追加描边矩形命令。
pub const IMPORT_STROKE_RECT: &str = "stroke_rect";
/// `draw_text(ptr, len, x, y, font, size, color)`，追加文本命令。
pub const IMPORT_DRAW_TEXT: &str = "draw_text";
/// `set_size(w, h)`，窗口内容尺寸（DIP）。
/// Optional compatibility import: set_corner_radius(radius) updates panel corner radius in DIP.
pub const IMPORT_SET_SIZE: &str = "set_size";
/// `set_panel(radius, shadow_radius, offset_x, offset_y, color)`.
/// Finite DIP bounds: corner radius 0..=4096, shadow radius 0..=250,
/// offsets -1024..=1024. Shadow bounds are conservative host resource limits;
/// Microsoft's DropShadow.BlurRadius reference does not specify a maximum.
pub const IMPORT_SET_PANEL: &str = "set_panel";
pub const IMPORT_SET_BACKDROP: &str = "set_backdrop";

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
/// `time_ms() -> f64`，Unix 纪元毫秒。
pub const IMPORT_TIME_MS: &str = "time_ms";
/// `log(ptr, len)`，诊断文本进入 notices。
pub const IMPORT_LOG: &str = "log";

/// AssemblyScript 运行时断言/abort 导入的模块名与函数名：
/// `env.abort(message, fileName, line, column)`，前两项是 AS UTF-16 对象指针。
pub const IMPORT_ABORT_MODULE: &str = "env";
pub const IMPORT_ABORT: &str = "abort";

// ── 字体槽位（host 侧 DirectWrite 文本格式，步骤 2 映射）────────────
/// 正文（Microsoft YaHei UI）。
pub const FONT_TEXT: i32 = 0;
/// 候选序号（Segoe UI）。
pub const FONT_NUMBER: i32 = 1;
/// 注释/次要文本。
pub const FONT_COMMENT: i32 = 2;
/// MDL2 图标（Segoe MDL2 Assets）。
pub const FONT_ICON: i32 = 3;
/// Bold primary text; inherits the family of FONT_TEXT.
pub const FONT_TEXT_BOLD: i32 = 4;

// ── 动作 id（对应 theme_api::UiAction）──────────────────────────────
/// 选中第 index 个候选项。
pub const ACTION_ITEM: i32 = 0;
/// 上一页。
pub const ACTION_PREVIOUS: i32 = 1;
/// 下一页。
pub const ACTION_NEXT: i32 = 2;
/// 打开表情面板。
pub const ACTION_EMOJI: i32 = 3;

// ── 鼠标类型 ─────────────────────────────────────────────────────────
pub const MOUSE_DOWN: i32 = 0;
pub const MOUSE_MOVE: i32 = 1;
pub const MOUSE_UP: i32 = 2;
pub const MOUSE_LEAVE: i32 = 3;

// ── init 的 mode 参数 ────────────────────────────────────────────────
pub const MODE_LIVE: i32 = 0;
pub const MODE_PREVIEW: i32 = 1;

// ── init/render 返回的错误码 ─────────────────────────────────────────
pub const ERR_OK: i32 = 0;
/// 当前视图不可用。
pub const ERR_BAD_VIEW: i32 = 1;
/// 主题内部错误。
pub const ERR_INTERNAL: i32 = 2;

/// 单次从 guest 内存读取的字符串上限；各导入另有更小的限额。
pub const MAX_STRING_BYTES: usize = 65_536;

/// Native rounded-panel presentation, independent of guest drawing commands.
/// Coordinates are DIP and color is 0xAARRGGBB. Zero shadow radius disables shadow.
/// This stage describes native shadow and surface offset only; guest drawing owns
/// the background. Future presentation capabilities require an explicit ABI extension.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PanelStyle {
    pub corner_radius: f32,
    pub shadow_radius: f32,
    pub offset_x: f32,
    pub offset_y: f32,
    pub color: u32,
}

/// 单条绘制命令；坐标为 DIP，颜色为 0xAARRGGBB。
#[derive(Debug, Clone, PartialEq)]
pub enum DrawCommand {
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
    Text {
        x: f32,
        y: f32,
        font: i32,
        size: f32,
        color: u32,
        text: String,
        glow: (f32, u32),
    },
}

impl DrawCommand {
    /// 所有浮点参数是否有限：NaN/Inf 命令会被 canvas 回放时跳过
    /// （D2D 对非有限坐标的行为未定义）。
    pub fn is_finite(&self) -> bool {
        match self {
            DrawCommand::FillRoundedRect {
                x, y, w, h, radius, ..
            } => [x, y, w, h, radius].iter().all(|v| v.is_finite()),
            DrawCommand::FillRect { x, y, w, h, .. } => [x, y, w, h].iter().all(|v| v.is_finite()),
            DrawCommand::StrokeRect {
                x, y, w, h, width, ..
            } => [x, y, w, h, width].iter().all(|v| v.is_finite()),
            DrawCommand::Text { x, y, size, .. } => [x, y, size].iter().all(|v| v.is_finite()),
        }
    }
}
