// weasel WASM 主题示例：候选列表。
//
// 布局：每行 = 序号 + 正文 + 拼音注释；选中行高亮、鼠标悬停行淡色高亮；
// 点击有效候选项发送 ACTION_ITEM（页内索引）。演示了 ABI 的全部主要路径：
// init / abi_version / render / mouse / frame / hide / refresh。
//
// 每次事件（render/mouse/frame/refresh）后宿主都读取最新命令流与 set_size，
// 因此每帧都完整重发全部绘制命令。

import {
  ACTION_ITEM,
  ERR_BAD_VIEW,
  ERR_OK,
  FONT_COMMENT,
  FONT_NUMBER,
  FONT_TEXT,
  MODE_LIVE,
  MOUSE_DOWN,
  MOUSE_UP,
  MOUSE_LEAVE,
  draw_text,
  fill_rect,
  log,
  measure_text,
  set_size,
  send_action,
  stroke_rect,
} from "../assembly/host";
import { View, readView } from "../assembly/view";
import { options } from "../assembly/data";

// ── 布局常量（DIP）──────────────────────────────────────────────
/** 窗口内边距。导出供宿主侧测试对齐布局。 */
export const PAD: i32 = 6;
/** 单行高度。导出供宿主侧测试对齐布局。 */
export const ITEM_H: i32 = 30;
let SIZE_TEXT: f32 = 15.0;
const SIZE_NUM: f32 = 12.0;
const SIZE_COMMENT: f32 = 12.0;
const GAP: f32 = 6.0; // 序号/正文/注释间距
const MIN_W: f32 = 160.0;
const MAX_W: f32 = 480.0;



// ── 状态 ─────────────────────────────────────────────────────────
let dark: bool = false;
let live: bool = true;
let view: View | null = null;
let hoverRow: i32 = -1;
let pressedRow: i32 = -1;
let layoutWidth: f32 = 0;

// ── 调色板（0xAARRGGBB）──────────────────────────────────────────
class Palette {
  bg: u32 = 0;
  border: u32 = 0;
  hover: u32 = 0;
  selected: u32 = 0;
  text: u32 = 0;
  secondary: u32 = 0;
  number: u32 = 0;
}
const pal = new Palette();

function applyPalette(): void {
  if (dark) {
    pal.bg = 0xff1f2328;
    pal.border = 0xff3d444b;
    pal.hover = 0xff2c3238;
    pal.selected = 0xff274060;
    pal.text = 0xffe6edf3;
    pal.secondary = 0xff8b949e;
    pal.number = 0xff7d8590;
  } else {
    pal.bg = 0xfff7f8fa;
    pal.border = 0xffc9d1d9;
    pal.hover = 0xffe8ecf0;
    pal.selected = 0xffd6e4ff;
    pal.text = 0xff24292f;
    pal.secondary = 0xff6e7781;
    pal.number = 0xff8c959f;
  }
}

// ── 字符串 → (ptr, len) 助手 ──────────────────────────────────────
// AS 0.28 字符串内部为 UTF-16，宿主按 UTF-8 读取，故用 String.UTF8.encode
// 转码到线性内存；返回的 ArrayBuffer 指针即 UTF-8 数据首址。
function logf(s: string): void {
  const bytes = String.UTF8.encode(s);
  log(changetype<i32>(bytes), bytes.byteLength);
}

function measure(s: string, font: i32, size: f32): f32 {
  const bytes = String.UTF8.encode(s);
  return measure_text(changetype<i32>(bytes), bytes.byteLength, font, size);
}

function draw(s: string, x: f32, y: f32, font: i32, size: f32, color: u32): void {
  // Keep the managed buffer alive across the host call. Other arguments have
  // already been evaluated, so nested measurements cannot collect this buffer.
  const bytes = String.UTF8.encode(s);
  draw_text(changetype<i32>(bytes), bytes.byteLength, x, y, font, size, color);
}

// ── 绘制 ─────────────────────────────────────────────────────────
function layoutAndDraw(): void {
  const v = view;
  if (v == null || v.items.length == 0) {
    // 无候选：最小占位（宿主在 visible=false 时本就不调 render）
    set_size(MIN_W, 32.0);
    fill_rect(0.0, 0.0, MIN_W, 32.0, pal.bg);
    stroke_rect(0.0, 0.0, MIN_W, 32.0, pal.border, 1.0);
    return;
  }
  const n = v.items.length;

  // 第一遍：测量全部文本，记录每行“正文+注释”宽度与序号列宽
  const textWidths: f32[] = [];
  let numW: f32 = 0.0;
  for (let i = 0; i < n; i++) {
    const item = v.items[i];
    const num = (v.pageStart + i + 1).toString();
    const nw = measure(num, FONT_NUMBER, SIZE_NUM);
    const pw = measure(item.primary, FONT_TEXT, SIZE_TEXT);
    const cw =
      item.secondary.length == 0
        ? <f32>0.0
        : measure(item.secondary, FONT_COMMENT, SIZE_COMMENT);
    numW = nw > numW ? nw : numW;
    textWidths.push(pw + (cw > 0.0 ? GAP + cw : <f32>0));
  }
  let maxTextW: f32 = 0.0;
  for (let i = 0; i < n; i++) {
    maxTextW = textWidths[i] > maxTextW ? textWidths[i] : maxTextW;
  }

  const textX: f32 = <f32>(PAD) + numW + GAP;
  let width: f32 = textX + maxTextW + <f32>(PAD);
  if (width < MIN_W) width = MIN_W;
  if (width > MAX_W) width = MAX_W;
  const heightDip: i32 = 2 * PAD + ITEM_H * n;
  const height: f32 = <f32>heightDip;

  layoutWidth = width;
  set_size(width, height);
  fill_rect(0.0, 0.0, width, height, pal.bg);
  stroke_rect(0.0, 0.0, width, height, pal.border, 1.0);

  for (let i = 0; i < n; i++) {
    const item = v.items[i];
    const y = <f32>(PAD + ITEM_H * i);
    if (i == v.selectedIndex) {
      fill_rect(0.0, y, width, <f32>(ITEM_H), pal.selected);
    } else if (i == hoverRow) {
      fill_rect(0.0, y, width, <f32>(ITEM_H), pal.hover);
    }
    const num = (v.pageStart + i + 1).toString();
    const base = item.enabled ? pal.text : pal.secondary;
    draw(
      num,
      <f32>PAD,
      y + (<f32>(ITEM_H) - SIZE_NUM) / <f32>2.0,
      FONT_NUMBER,
      SIZE_NUM,
      i == v.selectedIndex ? pal.text : pal.number,
    );
    draw(
      item.primary,
      textX,
      y + (<f32>(ITEM_H) - SIZE_TEXT) / <f32>2.0,
      FONT_TEXT,
      SIZE_TEXT,
      base,
    );
    if (item.secondary.length > 0) {
      draw(
      item.secondary,
        textX + textWidths[i] - measure(item.secondary, FONT_COMMENT, SIZE_COMMENT),
        y + (<f32>(ITEM_H) - SIZE_COMMENT) / <f32>2.0,
        FONT_COMMENT,
        SIZE_COMMENT,
        pal.secondary,
      );
    }
  }
}

// ── 导出（宿主调用入口）──────────────────────────────────────────
/** 首帧前由宿主调用。mode: MODE_LIVE/MODE_PREVIEW；dark: 0/1。 */
export function init(mode: i32, darkFlag: i32): i32 {
  live = mode == MODE_LIVE;
  dark = darkFlag != 0;
  const configuredSize = options.number("/fontSize", 15.0);
  SIZE_TEXT = <f32>Math.min(28.0, Math.max(8.0, configuredSize));
  applyPalette();
  logf(live ? "wasm theme init (live)" : "wasm theme init (preview)");
  return ERR_OK;
}

/** Independent theme ABI, not the native DLL or RPC version. */
export function abi_version(): i32 { return 1; }

export function render(): i32 {
  const parsed = readView();
  if (parsed == null) return ERR_BAD_VIEW;
  view = parsed;
  hoverRow = -1;
  pressedRow = -1;
  layoutAndDraw();
  return ERR_OK;
}

/** 鼠标事件（窗口局部 DIP 坐标）：悬停高亮 + 点击选中。 */
export function mouse(kind: i32, x: f32, y: f32): void {
  const v = view;
  if (v == null || v.items.length == 0) return;
  let row = -1;
  const relY = y - <f32>PAD;
  const tableH = <f32>(ITEM_H) * <f32>v.items.length;
  if (kind != MOUSE_LEAVE && x >= 0.0 && x < layoutWidth && relY >= 0.0 && relY < tableH) {
    row = <i32>(relY / <f32>ITEM_H);
  }
  if (row != hoverRow) {
    hoverRow = row;
    layoutAndDraw();
  }
  if (kind == MOUSE_DOWN) pressedRow = row;
  if (kind == MOUSE_LEAVE) pressedRow = -1;
  if (kind == MOUSE_UP) {
    const pressed = pressedRow;
    pressedRow = -1;
    if (row >= 0 && row == pressed && v.items[row].enabled) send_action(ACTION_ITEM, row);
  }
}

/** 动画帧（Unix 毫秒）。示例无连续动画：不 request_frame，计时自然停止。 */
export function frame(now: f64): void {
  // 预留：此处可做选中动画；保持无操作即可。
}

/** 候选窗被宿主隐藏（如输入法失去焦点）。 */
export function hide(): void {
  view = null;
  hoverRow = -1;
  pressedRow = -1;
}

/** 系统外观变化（dark: 0/1）；有内容时重发命令流。 */
export function refresh(darkFlag: i32): void {
  dark = darkFlag != 0;
  applyPalette();
  if (view != null) {
    hoverRow = -1;
    layoutAndDraw();
  }
}
