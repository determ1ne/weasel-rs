import { fill_rounded_rect } from "./raw";
import { Font, TextLayout } from "./resources";

/**
 * `set_font` 返回的字体槽句柄。数值只标识 SDK 内部缓存，不具有跨主题语义。
 * 主题应保存返回值，不要自行约定全局的正文、序号或粗体槽位编号。
 */
export type FontSlot = i32;

class SlotCache {
  fonts: Map<u32, Font> = new Map<u32, Font>();
  layouts: Map<u32, Map<string, TextLayout>> = new Map<u32, Map<string, TextLayout>>();

  constructor(public family: string, public weight: i32) {}
}

// 槽位按整数直接索引；字号使用 f32 位模式索引，不再拼接字符串构造字体键。
const slots = new Map<i32, SlotCache>();
let fontCount: i32 = 0;
let layoutCount: i32 = 0;

function clearLayouts(): void {
  const caches = slots.values();
  for (let i = 0; i < caches.length; i++) {
    const groups = caches[i].layouts.values();
    for (let j = 0; j < groups.length; j++) {
      const entries = groups[j].values();
      for (let k = 0; k < entries.length; k++) entries[k].dispose();
    }
    caches[i].layouts.clear();
  }
  layoutCount = 0;
}

function clearFonts(): void {
  clearLayouts();
  const caches = slots.values();
  for (let i = 0; i < caches.length; i++) {
    const entries = caches[i].fonts.values();
    for (let j = 0; j < entries.length; j++) entries[j].dispose();
    caches[i].fonts.clear();
  }
  fontCount = 0;
}

/**
 * 配置一个缓存槽并返回字体句柄。字重范围为 1..999。
 *
 * 通常在 `theme_create` 中调用；重新配置已有槽位会清空派生字体和布局缓存。
 */
export function set_font(slot: i32, family: string, weight: i32 = 400): FontSlot {
  assert(slot >= 0);
  assert(family.length > 0);
  assert(weight >= 1 && weight <= 999);
  clearFonts();
  slots.set(slot, new SlotCache(family, weight));
  return slot;
}

function layout(font: FontSlot, text: string, size: f32): TextLayout {
  assert(slots.has(font));
  assert(isFinite(size) && size > 0);
  const cache = slots.get(font);
  const sizeKey = reinterpret<u32>(size);
  if (!cache.fonts.has(sizeKey)) {
    if (fontCount >= 64) clearFonts();
    const created = Font.create(cache.family, size, cache.weight);
    assert(created.valid);
    cache.fonts.set(sizeKey, created);
    fontCount++;
  }
  if (!cache.layouts.has(sizeKey)) cache.layouts.set(sizeKey, new Map<string, TextLayout>());
  let layouts = cache.layouts.get(sizeKey);
  if (!layouts.has(text)) {
    if (layoutCount >= 128) {
      clearLayouts();
      cache.layouts.set(sizeKey, new Map<string, TextLayout>());
      layouts = cache.layouts.get(sizeKey);
    }
    const created = TextLayout.create(cache.fonts.get(sizeKey), text);
    assert(created.valid);
    layouts.set(text, created);
    layoutCount++;
    return created;
  }
  return layouts.get(text);
}

/** 测量单行文本宽度，单位为 DIP。 */
export function measure_text(font: FontSlot, text: string, size: f32): f32 {
  return layout(font, text, size).width;
}

/** 返回样本 `M中` 的布局高度，单位为 DIP。 */
export function line_height(font: FontSlot, size: f32): f32 {
  return layout(font, "M中", size).height;
}

/** 使用缓存布局绘制文本。 */
export function draw_text(
  font: FontSlot,
  text: string,
  x: f32,
  y: f32,
  size: f32,
  color: u32,
): void {
  layout(font, text, size).draw(x, y, color);
}

/** 使用缓存布局绘制文本和外发光。 */
export function draw_text_glow(
  font: FontSlot,
  text: string,
  x: f32,
  y: f32,
  size: f32,
  color: u32,
  glowRadius: f32,
  glowColor: u32,
): void {
  layout(font, text, size).draw(x, y, color, glowRadius, glowColor);
}

export function roundedRect(x: f32, y: f32, w: f32, h: f32, radius: f32, color: u32): void {
  fill_rounded_rect(x, y, w, h, radius, color);
}
