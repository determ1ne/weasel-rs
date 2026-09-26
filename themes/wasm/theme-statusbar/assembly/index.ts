import {
  ABI_VERSION, Action, Capability, ErrorCode, EventKind, FrameResult, PointerPhase, View,
  begin_drag, draw_text, fill_rect, line_height, measure_text, options, readView, roundedRect,
  send_action, set_fixed_position, set_font, set_panel, set_size, set_visible, stroke_rect,
} from "@weasel-rs/sdk-as/assembly";
import {
  FONT_FACE, FONT_SIZE, POSITION_X, POSITION_Y, WIDTH,
} from "./defaults";

const HEIGHT: f32 = 72.0;
const HEADER: f32 = 36.0;
const CLOSE_Y: f32 = 5.0;
const CLOSE_W: f32 = 30.0;
const CLOSE_H: f32 = 27.0;
const PAD: f32 = 10.0;

let width: f32 = WIDTH;
let fontSize: f32 = FONT_SIZE;
let textY: f32 = 0.0;
let textFont: i32 = -1;
let numberFont: i32 = -1;
let boldFont: i32 = -1;
let view: View | null = null;
let dismissed: bool = false;
let previousAscii: bool = true;
let hoverClose: bool = false;
let pressedClose: bool = false;
let pressedItem: i32 = -1;
let pressedNext: bool = false;

class Hit {
  constructor(public x: f32, public w: f32, public index: i32) {}
}
let hits: Hit[] = [];

function clamp(value: f32, low: f32, high: f32): f32 {
  return Mathf.max(low, Mathf.min(high, value));
}

function configure(): void {
  width = clamp(<f32>options.number("/width", WIDTH), 320.0, 800.0);
  fontSize = clamp(<f32>options.number("/font_size", FONT_SIZE), 12.0, 28.0);
  const x = clamp(<f32>options.number("/position_x", POSITION_X), -8192.0, 8192.0);
  const y = clamp(<f32>options.number("/position_y", POSITION_Y), -8192.0, 8192.0);
  const family = options.string("/font_face", FONT_FACE);
  const resolved = family.length > 0 ? family : FONT_FACE;
  textFont = set_font(0, resolved, 400);
  numberFont = set_font(1, "Segoe UI", 400);
  boldFont = set_font(2, resolved, 700);
  set_fixed_position(x, y);
  set_panel(3.0, 7.0, 0.0, 2.0, 0x48005984);
  set_size(width, HEIGHT);
  textY = (HEADER - line_height(textFont, fontSize)) / 2.0;
}

// 用少量色带模拟参考图的浅蓝玻璃高光，不依赖位图资源。
function paintChrome(): void {
  roundedRect(0, 0, width, HEIGHT, 3, 0xff55bdf0);
  roundedRect(2, 2, width - 4, HEIGHT - 4, 2, 0xffeaf9ff);

  fill_rect(4, 4, width - 8, 8, 0xffdff6ff);
  fill_rect(4, 12, width - 8, 8, 0xffc9eeff);
  fill_rect(4, 20, width - 8, 8, 0xffb8e7fb);
  fill_rect(4, 28, width - 8, 7, 0xffa9def6);

  fill_rect(4, HEADER, width - 8, 8, 0xfff9fdff);
  fill_rect(4, HEADER + 8, width - 8, 12, 0xfff2faff);
  fill_rect(4, HEADER + 20, width - 8, 12, 0xffeaf6fc);

  // 左侧输入区的斜肩：逐行改变右边界，形成 b.png 中的折角轮廓。
  for (let row: i32 = 0; row < 31; row++) {
    const end: f32 = <f32>190 + <f32>row * <f32>1.25;
    const color: u32 = row < 9 ? 0xfff7fcff : row < 20 ? 0xffedf9ff : 0xffe4f5fd;
    fill_rect(5, 4 + <f32>row, end - <f32>5, <f32>1.1, color);
    fill_rect(end, 4 + <f32>row, <f32>1.2, <f32>1.1, 0xff75c9ef);
  }

  fill_rect(3, HEADER - 1, width - 6, 1, 0xff77c9ee);
  fill_rect(3, HEADER, width - 6, 1, 0xffffffff);
  stroke_rect(1, 1, width - 2, HEIGHT - 2, 0xff3eb5ed, 1);
  stroke_rect(4, 4, width - 8, HEIGHT - 8, 0xff9edcf7, 1);
}

function paintClose(): void {
  const x = width - 37.0;
  const outer = pressedClose ? 0xff63bada : hoverClose ? 0xff7bd2f1 : 0xff8bd8f4;
  roundedRect(x, CLOSE_Y, CLOSE_W, CLOSE_H, 13, 0xff61bfe9);
  roundedRect(x + 2, CLOSE_Y + 2, CLOSE_W - 4, CLOSE_H - 4, 11, outer);
  fill_rect(x + 7, CLOSE_Y + 4, CLOSE_W - 14, 2, 0x80ffffff);
  draw_text(boldFont, "×", x + 6.5, CLOSE_Y - 1.0, 23, 0xff087ebc);
}

function paintContent(): void {
  const current = view;
  if (current == null) return;
  hits = [];
  paintChrome();
  paintClose();

  if (current.hasPreedit && current.preedit.length > 0) {
    draw_text(boldFont, current.preedit, PAD, 3 + textY, fontSize, 0xff111111);
    let cursor = min(max(current.preeditCursor, 0), current.preedit.length);
    if (cursor > 0 && cursor < current.preedit.length &&
        current.preedit.charCodeAt(cursor) >= 0xdc00 &&
        current.preedit.charCodeAt(cursor) <= 0xdfff) cursor--;
    const caret = PAD + measure_text(boldFont, current.preedit.substring(0, cursor), fontSize);
    fill_rect(caret, 8, 1.4, 20, 0xff167dad);
  }

  let x = PAD;
  const y = HEADER + (HEIGHT - HEADER - line_height(textFont, fontSize)) / 2.0 - 1.0;
  const right = width - 28.0;
  for (let i = 0; i < current.items.length && i < 9; i++) {
    const item = current.items[i];
    const label = (current.pageStart + i + 1).toString() + ".";
    const labelWidth = measure_text(numberFont, label, fontSize - 1);
    const itemFont = i == current.selectedIndex ? boldFont : textFont;
    const textWidth = measure_text(itemFont, item.primary, fontSize);
    const itemWidth = labelWidth + textWidth + 12.0;
    if (x + itemWidth > right) break;
    const color: u32 = item.enabled ? 0xff202020 : 0xff808080;
    draw_text(numberFont, label, x, y + 1, fontSize - 1, color);
    draw_text(itemFont, item.primary, x + labelWidth + 2, y, fontSize, color);
    hits.push(new Hit(x, itemWidth, i));
    x += itemWidth;
  }
  if (current.canPageNext || current.items.length > 0) {
    draw_text(textFont, "▶", width - 20, HEADER + 7, 13, 0xff101010);
  }
}

export function theme_abi_version(): i32 { return ABI_VERSION; }
export function theme_capabilities(): i32 { return Capability.Preedit | Capability.Resident; }

// 通知宿主：即使没有候选，也需要收到焦点和中英文模式快照。


export function theme_create(_mode: i32, _dark: i32): i32 {
  configure();
  set_visible(0);
  return ErrorCode.Success;
}

function render(): i32 {
  const next = readView();
  if (next == null) return ErrorCode.InvalidArgument;
  view = next;
  const chinese = next.active && next.hasAsciiMode && !next.asciiMode;
  if (!chinese) {
    previousAscii = true;
    dismissed = false;

    set_visible(0);

    return FrameResult.Present;
  }
  // 模式重新进入中文或开始实际输入时，恢复被关闭的状态栏。
  if (previousAscii || next.hasPreedit || next.items.length > 0) dismissed = false;
  previousAscii = false;
  if (dismissed) {

    set_visible(0);

    return FrameResult.Present;
  }
  hoverClose = false;
  pressedClose = false;
  pressedItem = -1;
  pressedNext = false;
  set_visible(1);
  paintContent();
  return FrameResult.Present;
}

function inside(x: f32, y: f32, left: f32, top: f32, w: f32, h: f32): bool {
  return x >= left && x < left + w && y >= top && y < top + h;
}

function mouse(kind: i32, x: f32, y: f32): i32 {
  let result = FrameResult.Keep;
  if (view == null || !isFinite(x) || !isFinite(y)) return FrameResult.Keep;
  const close = inside(x, y, width - 37, CLOSE_Y, CLOSE_W, CLOSE_H);
  const next = inside(x, y, width - 28, HEADER, 25, HEIGHT - HEADER);
  let item: i32 = -1;
  if (y >= HEADER && y < HEIGHT) {
    for (let i = 0; i < hits.length; i++) {
      if (x >= hits[i].x && x < hits[i].x + hits[i].w) { item = hits[i].index; break; }
    }
  }
  if (kind == PointerPhase.Move && hoverClose != close) {
    hoverClose = close;
    paintContent(); result = FrameResult.Present;
  }
  if (kind == PointerPhase.Down) {
    // 上层输入区（关闭按钮除外）作为整条状态栏的拖动把手。
    if (y >= 0 && y < HEADER && !close) {
      pressedClose = false;
      pressedNext = false;
      pressedItem = -1;
      begin_drag();
      return FrameResult.Keep;
    }
    pressedClose = close;
    pressedNext = next;
    pressedItem = item;
    if (close) paintContent(); result = FrameResult.Present;
  } else if (kind == PointerPhase.Up) {
    const closeClick = pressedClose && close;
    const nextClick = pressedNext && next;
    const itemClick = pressedItem >= 0 && pressedItem == item;
    pressedClose = false;
    pressedNext = false;
    pressedItem = -1;
    if (closeClick) {
      dismissed = true;

    set_visible(0); result = FrameResult.Present;

      send_action(Action.Dismiss, 0);
    } else if (nextClick && view!.canPageNext) {
      send_action(Action.Next, 0);
    } else if (itemClick) {
      send_action(Action.Item, item);
    }
  } else if (kind == PointerPhase.Leave || kind == PointerPhase.Cancel) {
    hoverClose = false;
    pressedClose = false;
    pressedNext = false;
    pressedItem = -1;
    paintContent(); result = FrameResult.Present;
  }
  return result;
}

function frame(_now: f64): void {}
function hide(): i32 {
  view = null;
  hits = [];
  dismissed = false;
  previousAscii = true;

    set_visible(0);

  return FrameResult.Present;
}
function refresh(_dark: i32): i32 { paintContent();   return view == null ? FrameResult.Keep : FrameResult.Present;
}

// host负责事务；只有完整绘制才返回Present，动作或无变化返回Keep。
export function theme_event(kind: i32, detail: i32, x: f32, y: f32, now: f64): i32 {
  switch (kind) {
    case EventKind.View: return render();
    case EventKind.Appearance: return refresh(detail);
    case EventKind.Hide: return hide();
    case EventKind.Pointer: return mouse(detail, x, y);
    case EventKind.Animation: frame(now); return FrameResult.Keep;
    default: return ErrorCode.InvalidArgument;
  }
}
