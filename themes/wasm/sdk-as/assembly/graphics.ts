import { draw_text, measure_text, fill_rounded_rect } from "./host";

// Guest buffers remain rooted until the synchronous host call completes.
export function measure(text: string, font: i32, size: f32): f32 {
  const bytes = String.UTF8.encode(text);
  return measure_text(changetype<i32>(bytes), bytes.byteLength, font, size);
}

export function draw(text: string, x: f32, y: f32, font: i32, size: f32, color: u32): void {
  const bytes = String.UTF8.encode(text);
  draw_text(changetype<i32>(bytes), bytes.byteLength, x, y, font, size, color);
}

// One native primitive avoids antialiased seams between adjacent strips.
// This does not make the native window's outer corners transparent.
export function roundedRect(x: f32, y: f32, w: f32, h: f32, radius: f32, color: u32): void {
  fill_rounded_rect(x, y, w, h, radius, color);
}
