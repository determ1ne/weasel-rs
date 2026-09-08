// 主题生成绘制命令；窗口、DPI、配置合并与 JSON 解析由宿主负责。
import {
  View, readView, draw, measure, roundedRect, set_size,
  send_action, ACTION_ITEM, FONT_TEXT, FONT_NUMBER, FONT_COMMENT,
  MOUSE_DOWN, MOUSE_UP, MOUSE_LEAVE, ERR_OK, ERR_BAD_VIEW,
} from "@weasel-rs/sdk-as/assembly";
export { default_config } from "./defaults";

// 配置合并后、init 前探测能力；支持编码和候选预览两种 preedit。
export function probe_preedit(_required: i32): i32 {
  return ERR_OK;
}

import { loadConfig, colors, PAD, ROW_HEIGHT, GAP, TEXT_SIZE, LABEL_SIZE, COMMENT_SIZE,
  RADIUS, OUTER_RADIUS, MIN_WIDTH, MAX_WIDTH, PAD_Y, ROW_GAP, BORDER, LABEL_GAP,
  PADDING, MIN_HEIGHT, MAX_HEIGHT, TEXT_HEIGHT, LABEL_HEIGHT, COMMENT_HEIGHT, HORIZONTAL, PREVIEW, label } from "./style";

let view: View | null = null;
let width: f32 = MIN_WIDTH;
let height: f32 = 0;
let labelWidth: f32 = 0;
let primaryWidths: f32[] = [];
// 同一快照内缓存序号及宽度，避免悬停重绘时重复测量。
let labels: string[] = [];
let labelWidths: f32[] = [];
let hover: i32 = -1;
let pressed: i32 = -1;

class ItemRect {
  constructor(public x: f32, public y: f32, public w: f32) {}
}
let rects: ItemRect[] = [];
let preeditText: string = "";
let preeditWidth: f32 = 0;
let caretX: f32 = 0;
let caretWidth: f32 = 0;
let preeditBefore: string = "";
let preeditAfter: string = "";
let preeditHeight: f32 = 0;
let candidateY: f32 = 0;

// 绘制与命中共用裁剪规则，超出配置尺寸的候选不能点击。
function visibleRect(r: ItemRect): bool {
  return r.y + ROW_HEIGHT <= height - PAD_Y && r.x + r.w <= width - PAD;
}

// 坐标为 DIP；横排按项宽布局，竖排共享序号列宽。
function layout(v: View): void {
  labelWidth = 0;
  primaryWidths = [];
  labels = [];
  labelWidths = [];
  rects = [];
  const itemWidths: f32[] = [];
  let contentWidth: f32 = 0;
  for (let i = 0; i < v.items.length; i++) {
    const item = v.items[i];
    const number = label(i);
    const numberWidth = measure(number, FONT_NUMBER, LABEL_SIZE);
    labels.push(number);
    labelWidths.push(numberWidth);
    labelWidth = Mathf.max(labelWidth, numberWidth);
    const primary = measure(item.primary, FONT_TEXT, TEXT_SIZE);
    primaryWidths.push(primary);
    const comment = item.secondary.length > 0 ? GAP + measure(item.secondary, FONT_COMMENT, COMMENT_SIZE) : <f32>0;
    itemWidths.push(2 * PADDING + numberWidth + LABEL_GAP + primary + comment);
    contentWidth = Mathf.max(contentWidth, primary + comment);
  }
  preeditText = "";
  caretX = 0;
  caretWidth = 0;
  preeditBefore = "";
  preeditAfter = "";
  if (v.hasPreedit) {
    preeditText = v.preedit;
    if (PREVIEW && v.selectedIndex >= 0 && v.selectedIndex < v.items.length
        && v.items[v.selectedIndex].enabled) preeditText = v.items[v.selectedIndex].primary;
    let cursor = PREVIEW ? preeditText.length : min(max(v.preeditCursor, 0), preeditText.length);
    // 光标以 UTF-16 单元计数，切分时不能从代理对中间切开。
    if (cursor > 0 && cursor < preeditText.length &&
        preeditText.charCodeAt(cursor) >= 0xdc00 && preeditText.charCodeAt(cursor) <= 0xdfff) cursor--;
    preeditBefore = preeditText.substring(0, cursor);
    preeditAfter = preeditText.substring(cursor);
    caretX = measure(preeditBefore, FONT_TEXT, TEXT_SIZE);
    if (!PREVIEW) caretWidth = measure("^", FONT_TEXT, TEXT_SIZE);
  }
  // ^ 独占水平槽位并向下偏移，不能覆盖后面的编码字符。
  preeditWidth = PREVIEW ? measure(preeditText, FONT_TEXT, TEXT_SIZE)
    : caretX + caretWidth + measure(preeditAfter, FONT_TEXT, TEXT_SIZE);
  preeditHeight = TEXT_HEIGHT + (PREVIEW ? 0 : TEXT_SIZE * 0.35);
  candidateY = PAD_Y + (v.hasPreedit ? preeditHeight + 2 * PADDING + GAP : 0);
  let candidatesWidth: f32 = 2 * PADDING + labelWidth + LABEL_GAP + contentWidth;
  if (HORIZONTAL) {
    candidatesWidth = 0;
    for (let i = 0; i < itemWidths.length; i++) candidatesWidth += itemWidths[i] + (i > 0 ? ROW_GAP : 0);
  }
  width = Mathf.min(MAX_WIDTH, Mathf.max(MIN_WIDTH,
    2 * PAD + Mathf.max(candidatesWidth, v.hasPreedit ? preeditWidth + 2 * PADDING + LABEL_GAP : 0)));
  const rows: f32 = HORIZONTAL ? (v.items.length > 0 ? 1 : 0) : <f32>v.items.length;
  height = Mathf.min(MAX_HEIGHT, Mathf.max(MIN_HEIGHT,
    candidateY + PAD_Y + ROW_HEIGHT * rows + ROW_GAP * Mathf.max(0, rows - 1)));
  let x = PAD;
  for (let i = 0; i < v.items.length; i++) {
    rects.push(new ItemRect(HORIZONTAL ? x : PAD,
      candidateY + (HORIZONTAL ? 0 : <f32>i * (ROW_HEIGHT + ROW_GAP)),
      HORIZONTAL ? itemWidths[i] : width - 2 * PAD));
    x += itemWidths[i] + ROW_GAP;
  }
  set_size(width, height);
}

function paint(): void {
  const v = view;
  if (v == null) return;
  // 外层边框、内层背景；圆角之外保持透明，阴影交由宿主合成。
  roundedRect(0, 0, width, height, OUTER_RADIUS, colors.border_color);
  const border = Mathf.min(BORDER, Mathf.min(width, height) / 2);
  roundedRect(border, border, width - 2 * border, height - 2 * border,
    Mathf.max(0, OUTER_RADIUS - border), colors.back_color);
  const highlighted = hover >= 0 ? hover : v.selectedIndex;
  if (v.hasPreedit) {
    // 编码模式高亮输入段；预览模式显示有效候选，不绘制编码光标。
    if (!PREVIEW && preeditWidth > 0) roundedRect(PAD, PAD_Y, preeditWidth + 2 * PADDING,
      preeditHeight + 2 * PADDING, RADIUS, colors.hilited_back);
    if (PREVIEW) {
      draw(preeditText, PAD + PADDING, PAD_Y + PADDING, FONT_TEXT, TEXT_SIZE, colors.text);
    } else {
      draw(preeditBefore, PAD + PADDING, PAD_Y + PADDING, FONT_TEXT, TEXT_SIZE, colors.hilited_text);
      draw("^", PAD + PADDING + caretX, PAD_Y + PADDING + TEXT_SIZE * 0.35,
        FONT_TEXT, TEXT_SIZE, colors.hilited_text);
      draw(preeditAfter, PAD + PADDING + caretX + caretWidth, PAD_Y + PADDING,
        FONT_TEXT, TEXT_SIZE, colors.hilited_text);
    }
  }
  for (let i = 0; i < v.items.length; i++) {
    const item = v.items[i];
    const rect = rects[i];
    const y = rect.y;
    if (!visibleRect(rect)) continue;
    const numberWidth = HORIZONTAL ? labelWidths[i] : labelWidth;
    const textX = rect.x + PADDING + numberWidth + LABEL_GAP;
    const selected = i == highlighted && item.enabled;
    if (selected) roundedRect(rect.x, y, rect.w, ROW_HEIGHT, RADIUS, colors.hilited_candidate_back_color);
    draw(labels[i], rect.x + PADDING, y + (ROW_HEIGHT - LABEL_HEIGHT) / 2,
      FONT_NUMBER, LABEL_SIZE, selected ? colors.hilited_label_color : colors.label_color);
    draw(item.primary, textX, y + (ROW_HEIGHT - TEXT_HEIGHT) / 2,
      FONT_TEXT, TEXT_SIZE, selected ? colors.hilited_candidate_text_color : colors.candidate_text_color);
    if (item.secondary.length > 0) draw(item.secondary, textX + primaryWidths[i] + GAP,
      y + (ROW_HEIGHT - COMMENT_HEIGHT) / 2, FONT_COMMENT, COMMENT_SIZE,
      selected ? colors.hilited_comment_text_color : colors.comment_text_color);
  }
}

export function abi_version(): i32 { return 1; }
export function init(_mode: i32, _dark: i32): i32 { loadConfig(); return ERR_OK; }
export function render(): i32 {
  const next = readView();
  if (next == null) return ERR_BAD_VIEW;
  view = next;
  hover = -1;
  pressed = -1;
  layout(next);
  paint();
  return ERR_OK;
}

export function mouse(kind: i32, x: f32, y: f32): void {
  // 悬停仅改变视觉；同一有效候选上按下、抬起才向引擎提交动作。
  const v = view;
  if (v == null) return;
  let row: i32 = -1;
  if (kind != MOUSE_LEAVE) {
    for (let i = 0; i < rects.length; i++) {
      const r = rects[i];
      if (visibleRect(r) &&
          x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + ROW_HEIGHT &&
          v.items[i].enabled) { row = i; break; }
    }
  }
  if (hover != row) { hover = row; paint(); }
  if (kind == MOUSE_DOWN) pressed = row;
  if (kind == MOUSE_LEAVE) pressed = -1;
  if (kind == MOUSE_UP) {
    const target = pressed;
    pressed = -1;
    if (row >= 0 && row == target) send_action(ACTION_ITEM, row);
  }
}
export function frame(_now: f64): void {}
export function hide(): void {
  view = null;
  primaryWidths = [];
  labels = [];
  labelWidths = [];
  rects = [];
  hover = -1;
  pressed = -1;
}
// 固定配置配色，不随系统深浅色覆盖；刷新仅重绘当前快照。
export function refresh(_dark: i32): void { paint(); }
