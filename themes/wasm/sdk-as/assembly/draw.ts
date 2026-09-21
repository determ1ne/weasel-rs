/**
 * 仅 theme_event 内使用；每次 Present 提交完整画面，而非追加。
 * push_transform / push_clip 必须用 pop_draw_state 配对。
 */
export { fill_rect, fill_rounded_rect, stroke_rect, push_transform, push_clip, pop_draw_state } from "./raw";
export { draw, measure, line_height, roundedRect } from "./graphics";
import { set_font as setFontRaw } from "./graphics";
/** 便捷字体槽配置，不需要调用者处理 UTF-8 指针；切换字体会清理布局缓存。 */
export function set_font(slot:i32, family:string):void {
  const bytes=String.UTF8.encode(family);
  setFontRaw(slot,changetype<i32>(bytes),bytes.byteLength);
}
