import {Action,PointerPhase,Mode,ErrorCode} from "./types";
export * from "./types";
// ABI常量。低层签名和注释由abi.json生成；生命周期说明见index.ts。
export * from "./raw";

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
export const ACTION_ITEM: i32 = Action.Item;
/** 上一页。 */
export const ACTION_PREVIOUS: i32 = Action.Previous;
/** 下一页。 */
export const ACTION_NEXT: i32 = Action.Next;
/** 打开表情面板。 */
export const ACTION_EMOJI: i32 = Action.Emoji;
/** 取消当前 composition。 */
export const ACTION_DISMISS: i32 = Action.Dismiss;

// ── 鼠标类型 ─────────────────────────────────────────────────────
export const MOUSE_DOWN: i32 = PointerPhase.Down;
export const MOUSE_MOVE: i32 = PointerPhase.Move;
export const MOUSE_UP: i32 = PointerPhase.Up;
export const MOUSE_LEAVE: i32 = PointerPhase.Leave;
/** 捕获丢失或宿主取消；清除按下状态，不执行点击动作。 */
export const MOUSE_CANCEL: i32 = PointerPhase.Cancel;

// ── init 的 mode 参数 ────────────────────────────────────────────
export const MODE_LIVE: i32 = Mode.Live;
export const MODE_PREVIEW: i32 = Mode.Preview;

// ── init/render 返回的错误码 ─────────────────────────────────────
export const ERR_OK: i32 = ErrorCode.Success;
/** 当前视图不可用。 */
export const ERR_BAD_VIEW: i32 = ErrorCode.InvalidArgument;
/** 主题内部错误。 */
export const ERR_INTERNAL: i32 = ErrorCode.Internal;
