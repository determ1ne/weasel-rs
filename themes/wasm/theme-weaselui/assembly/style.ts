import * as defaults from "./defaults";
import { options, settings, BOOLEAN, STRING, NUMBER, log, set_font, line_height, set_panel } from "@weasel-rs/sdk-as/assembly";

function warn(path: string): void {
  const bytes = String.UTF8.encode("invalid weaselui config " + path + "; using default");
  log(changetype<i32>(bytes), bytes.byteLength);
}
// options 已由宿主合并默认值与用户覆盖；这里只校验字段，不自行解析 JSON。
// 缺失字段静默回退，类型或范围错误记录日志后回退。
function dimension(key: string, fallback: f32, minimum: f64 = 0, maximum: f64 = 4096): f32 {
  const path = "/layout/" + key;
  if (options.kind(path) == 0) return fallback;
  const value = options.number(path, -1);
  if (options.kind(path) != NUMBER || !isFinite(value) || value < minimum || value > maximum) {
    warn(path); return fallback;
  }
  return <f32>value;
}
// 接受 RGB / ARGB 十六进制（可带 #），统一转换为宿主使用的 ARGB。
function color(key: string, fallback: u32): u32 {
  const path = "/color/" + key;
  if (options.kind(path) == 0) return fallback;
  let text = options.string(path);
  if (text.startsWith("#")) text = text.substring(1);
  if (options.kind(path) != STRING || (text.length != 6 && text.length != 8)) {
    warn(path); return fallback;
  }
  let value: u32 = 0;
  for (let i = 0; i < text.length; i++) {
    const ch = text.charCodeAt(i);
    let digit: i32 = -1;
    if (ch >= 48 && ch <= 57) digit = ch - 48;
    else if (ch >= 65 && ch <= 70) digit = ch - 55;
    else if (ch >= 97 && ch <= 102) digit = ch - 87;
    if (digit < 0) { warn(path); return fallback; }
    value = (value << 4) | <u32>digit;
  }
  return text.length == 6 ? value | 0xff000000 : value;
}

// 字段名对应传统 Weasel 配色；保留独立字段，允许各类文字分别覆盖。
export class Colors {
  text: u32 = defaults.COLOR_TEXT;
  hilited_text: u32 = defaults.COLOR_HILITED_TEXT;
  hilited_back: u32 = defaults.COLOR_HILITED_BACK;
  back_color: u32 = defaults.COLOR_BACK;
  border_color: u32 = defaults.COLOR_BORDER;
  candidate_text_color: u32 = defaults.COLOR_CANDIDATE_TEXT;
  label_color: u32 = defaults.COLOR_LABEL;
  comment_text_color: u32 = defaults.COLOR_COMMENT_TEXT;
  hilited_candidate_text_color: u32 = defaults.COLOR_HILITED_CANDIDATE_TEXT;
  hilited_candidate_back_color: u32 = defaults.COLOR_HILITED_CANDIDATE_BACK;
  hilited_label_color: u32 = defaults.COLOR_HILITED_LABEL;
  hilited_comment_text_color: u32 = defaults.COLOR_HILITED_COMMENT_TEXT;
}
export const colors = new Colors();
export let HORIZONTAL: bool = defaults.HORIZONTAL;
export let PREVIEW: bool = false;
export let PAD: f32 = defaults.LAYOUT_MARGIN_X;
export let PAD_Y: f32 = defaults.LAYOUT_MARGIN_Y;
export let ROW_HEIGHT: f32 = 0; // init 中由字体实际行高计算。
export let ROW_GAP: f32 = defaults.LAYOUT_CANDIDATE_SPACING;
export let BORDER: f32 = defaults.LAYOUT_BORDER_WIDTH;
export let GAP: f32 = defaults.LAYOUT_SPACING;
export let LABEL_GAP: f32 = defaults.LAYOUT_HILITE_SPACING;
export let PADDING: f32 = defaults.LAYOUT_HILITE_PADDING;
export let TEXT_SIZE: f32 = defaults.FONT_POINT * 4 / 3;
export let LABEL_SIZE: f32 = defaults.LABEL_FONT_POINT * 4 / 3;
export let COMMENT_SIZE: f32 = defaults.COMMENT_FONT_POINT * 4 / 3;
export let TEXT_HEIGHT: f32 = 0;
export let LABEL_HEIGHT: f32 = 0;
export let COMMENT_HEIGHT: f32 = 0;
export let LABEL_FORMAT: string = defaults.LABEL_FORMAT;
export let RADIUS: f32 = defaults.LAYOUT_HILITE_CORNER_RADIUS;
export let OUTER_RADIUS: f32 = defaults.LAYOUT_CORNER_RADIUS;
export let MIN_WIDTH: f32 = defaults.LAYOUT_MIN_WIDTH;
export let MAX_WIDTH: f32 = defaults.LAYOUT_MAX_WIDTH == 0 ? 8192 : defaults.LAYOUT_MAX_WIDTH;
export let MIN_HEIGHT: f32 = defaults.LAYOUT_MIN_HEIGHT;
export let MAX_HEIGHT: f32 = defaults.LAYOUT_MAX_HEIGHT == 0 ? 8192 : defaults.LAYOUT_MAX_HEIGHT;

function font(slot: i32, key: string, fallback: string): void {
  const path = "/" + key;
  let family = options.string(path, fallback);
  if (options.kind(path) != 0 && (options.kind(path) != STRING || family.length == 0 || family.length > 64)) {
    warn(path); family = fallback;
  }
  const bytes = String.UTF8.encode(family);
  set_font(slot, changetype<i32>(bytes), bytes.byteLength);
}
// 配置字号为 pt，绘制接口使用 DIP：1pt = 96/72 DIP，不在主题中乘显示器 DPI。
function point(key: string, fallback: f32): f32 {
  const path = "/" + key;
  if (options.kind(path) == 0) return fallback * 4 / 3;
  const value = options.number(path, -1);
  if (options.kind(path) != NUMBER || !isFinite(value) || value < 3 || value > 384) {
    warn(path); return fallback * 4 / 3;
  }
  return <f32>(value * 4 / 3);
}
export function label(index: i32): string {
  return LABEL_FORMAT.replace("%s", (index + 1).toString());
}
// 初始化时读取一次配置、设置字体并测量行高，绘制路径只消费已校验的值。
export function loadConfig(): void {
  HORIZONTAL = options.kind("/horizontal") == 0 ? defaults.HORIZONTAL : options.boolean("/horizontal");
  if (options.kind("/horizontal") != 0 && options.kind("/horizontal") != BOOLEAN) {
    warn("/horizontal"); HORIZONTAL = defaults.HORIZONTAL;
  }
  const preeditType = settings.string("/preedit_type", "composition");
  PREVIEW = preeditType == "preview";
  if ((settings.kind("/preedit_type") != 0 && settings.kind("/preedit_type") != STRING) ||
      (preeditType != "composition" && preeditType != "preview")) warn("global preedit_type");
  colors.text = color("text", defaults.COLOR_TEXT);
  colors.hilited_text = color("hilited_text", defaults.COLOR_HILITED_TEXT);
  colors.hilited_back = color("hilited_back", defaults.COLOR_HILITED_BACK);
  font(0, "font_face", defaults.FONT_FACE); font(1, "label_font_face", defaults.LABEL_FONT_FACE); font(2, "comment_font_face", defaults.COMMENT_FONT_FACE);
  TEXT_SIZE = point("font_point", defaults.FONT_POINT); LABEL_SIZE = point("label_font_point", defaults.LABEL_FONT_POINT); COMMENT_SIZE = point("comment_font_point", defaults.COMMENT_FONT_POINT);
  TEXT_HEIGHT = line_height(0, TEXT_SIZE);
  LABEL_HEIGHT = line_height(1, LABEL_SIZE);
  COMMENT_HEIGHT = line_height(2, COMMENT_SIZE);
  LABEL_FORMAT = options.string("/label_format", defaults.LABEL_FORMAT);
  if ((options.kind("/label_format") != 0 && options.kind("/label_format") != STRING) ||
      LABEL_FORMAT.length > 64 || LABEL_FORMAT.indexOf("%s") < 0) {
    warn("/label_format"); LABEL_FORMAT = defaults.LABEL_FORMAT;
  }
  colors.back_color = color("back", defaults.COLOR_BACK);
  colors.border_color = color("border", defaults.COLOR_BORDER);
  colors.candidate_text_color = color("candidate_text", defaults.COLOR_CANDIDATE_TEXT);
  colors.label_color = color("label", defaults.COLOR_LABEL);
  colors.comment_text_color = color("comment_text", defaults.COLOR_COMMENT_TEXT);
  colors.hilited_candidate_text_color = color("hilited_candidate_text", defaults.COLOR_HILITED_CANDIDATE_TEXT);
  colors.hilited_candidate_back_color = color("hilited_candidate_back", defaults.COLOR_HILITED_CANDIDATE_BACK);
  colors.hilited_label_color = color("hilited_label", defaults.COLOR_HILITED_LABEL);
  colors.hilited_comment_text_color = color("hilited_comment_text", defaults.COLOR_HILITED_COMMENT_TEXT);
  PAD = dimension("margin_x", defaults.LAYOUT_MARGIN_X);
  PAD_Y = dimension("margin_y", defaults.LAYOUT_MARGIN_Y);
  PADDING = dimension("hilite_padding", defaults.LAYOUT_HILITE_PADDING);
  BORDER = dimension("border_width", defaults.LAYOUT_BORDER_WIDTH);
  ROW_GAP = dimension("candidate_spacing", defaults.LAYOUT_CANDIDATE_SPACING);
  GAP = dimension("spacing", defaults.LAYOUT_SPACING);
  LABEL_GAP = dimension("hilite_spacing", defaults.LAYOUT_HILITE_SPACING);
  RADIUS = dimension("hilite_corner_radius", defaults.LAYOUT_HILITE_CORNER_RADIUS);
  OUTER_RADIUS = dimension("corner_radius", defaults.LAYOUT_CORNER_RADIUS);
  set_panel(OUTER_RADIUS, dimension("shadow_radius", defaults.LAYOUT_SHADOW_RADIUS, 0, 250),
    dimension("shadow_offset_x", defaults.LAYOUT_SHADOW_OFFSET_X, -1024, 1024), dimension("shadow_offset_y", defaults.LAYOUT_SHADOW_OFFSET_Y, -1024, 1024),
    <i32>color("shadow", defaults.COLOR_SHADOW));
  ROW_HEIGHT = Mathf.max(TEXT_HEIGHT, Mathf.max(LABEL_HEIGHT, COMMENT_HEIGHT)) + 2 * PADDING;
  // 最大尺寸为 0 表示不作配置限制，但仍遵守宿主可接受的尺寸上限。
  MIN_WIDTH = dimension("min_width", defaults.LAYOUT_MIN_WIDTH);
  MAX_WIDTH = dimension("max_width", defaults.LAYOUT_MAX_WIDTH);
  if (MAX_WIDTH == 0) MAX_WIDTH = 8192;
  MAX_WIDTH = Mathf.max(MAX_WIDTH, MIN_WIDTH);
  MIN_HEIGHT = dimension("min_height", defaults.LAYOUT_MIN_HEIGHT);
  MAX_HEIGHT = dimension("max_height", defaults.LAYOUT_MAX_HEIGHT);
  if (MAX_HEIGHT == 0) MAX_HEIGHT = 8192;
  MAX_HEIGHT = Mathf.max(MAX_HEIGHT, MIN_HEIGHT);
}
