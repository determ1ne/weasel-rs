// weasel 宿主函数声明与 ABI 常量。
//
// 契约唯一事实来源：themes/wasm/src/protocol.rs。改动任一侧必须同步另一侧。
// 约定：坐标单位 DIP（宿主负责物理像素换算）；颜色 0xAARRGGBB；
// 文本以 UTF-8 字节存放在线性内存，用 (ptr, len) 引用；
// 模块必须导出名为 memory 的线性内存（AssemblyScript 自动导出）。
//
// 宿主函数导入语法（AS 0.28）：@external(模块名, 函数名) + declare function。

@external("weasel", "measure_text")
export declare function measure_text(text: i32, len: i32, font: i32, size: f32): f32;

@external("weasel", "fill_rect")
export declare function fill_rect(x: f32, y: f32, w: f32, h: f32, color: u32): void;
@external("weasel", "fill_rounded_rect")
export declare function fill_rounded_rect(x: f32, y: f32, w: f32, h: f32, radius: f32, color: u32): void;

@external("weasel", "stroke_rect")
export declare function stroke_rect(x: f32, y: f32, w: f32, h: f32, color: u32, width: f32): void;

@external("weasel", "draw_text")
export declare function draw_text(text: i32, len: i32, x: f32, y: f32, font: i32, size: f32, color: u32): void;

@external("weasel", "set_size")
export declare function set_size(w: f32, h: f32): void;

@external("weasel", "send_action")
export declare function send_action(action: i32, index: i32): void;

@external("weasel", "request_frame")
export declare function request_frame(): void;

@external("weasel", "time_ms")
export declare function time_ms(): f64;

@external("weasel", "log")
export declare function log(msg: i32, len: i32): void;

// ── 字体槽位（宿主 DirectWrite 文本格式）──────────────────────────
/** 正文（Microsoft YaHei UI）。 */
export const FONT_TEXT: i32 = 0;
/** 候选序号（Segoe UI）。 */
export const FONT_NUMBER: i32 = 1;
/** 注释/次要文本。 */
export const FONT_COMMENT: i32 = 2;
/** 图标（Segoe MDL2 Assets）。 */
export const FONT_ICON: i32 = 3;
/** Bold primary text, inheriting the family of FONT_TEXT. */
export const FONT_TEXT_BOLD: i32 = 4;

// ── 动作 id（对应 theme_api::UiAction）───────────────────────────
/** 选中第 index 个候选项（页内索引）。 */
export const ACTION_ITEM: i32 = 0;
/** 上一页。 */
export const ACTION_PREVIOUS: i32 = 1;
/** 下一页。 */
export const ACTION_NEXT: i32 = 2;
/** 打开表情面板。 */
export const ACTION_EMOJI: i32 = 3;

// ── 鼠标类型 ─────────────────────────────────────────────────────
export const MOUSE_DOWN: i32 = 0;
export const MOUSE_MOVE: i32 = 1;
export const MOUSE_UP: i32 = 2;
export const MOUSE_LEAVE: i32 = 3;

// ── init 的 mode 参数 ────────────────────────────────────────────
export const MODE_LIVE: i32 = 0;
export const MODE_PREVIEW: i32 = 1;

// ── init/render 返回的错误码 ─────────────────────────────────────
export const ERR_OK: i32 = 0;
/** 当前视图不可用。 */
export const ERR_BAD_VIEW: i32 = 1;
/** 主题内部错误。 */
export const ERR_INTERNAL: i32 = 2;

/** Set a font family for slot 0..3; measurements and drawing share the same family. */
@external("weasel", "set_font")
export declare function set_font(slot: i32, ptr: i32, len: i32): void;
/** Actual DirectWrite layout height in DIP, not the em size. */
@external("weasel", "line_height")
export declare function line_height(slot: i32, size: f32): f32;

/** Compatibility setter for panel corner radius in DIP; preserves shadow settings. */
@external("weasel", "set_corner_radius")
export declare function set_corner_radius(radius: f32): void;

/** Native panel: finite DIP corner radius 0..4096, shadow radius 0..250,
 * offsets -1024..1024, ARGB color. These are host contract bounds.
 * Zero shadow radius disables shadow. set_corner_radius preserves shadow settings. */
@external("weasel", "set_panel")
export declare function set_panel(radius: f32, shadow_radius: f32, offset_x: f32, offset_y: f32, color: i32): void;

/** Soft text halo for subsequent draws; radius 0..4 DIP, zero disables. */
@external("weasel", "set_text_glow")
export declare function set_text_glow(radius: f32, color: i32): void;

/** Persistent glass backdrop. Sigma 0..64 DIP; nonnegative weights must sum to 1.
 * The host uses opaque fallback_color when host backdrop is unavailable. */
@external("weasel", "set_backdrop")
export declare function set_backdrop(enabled: i32, tint: i32, blur_sigma: f32,
  backdrop_balance: f32, afterglow_balance: f32, color_balance: f32, fallback_color: i32): void;
