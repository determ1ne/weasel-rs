// In-memory ABI fixture shared by the SDK example and independent themes.
// Text measurement is approximate; this does not replace native visual testing.
export async function createHost(bytes, options = {}, settings = {}) {
  let instance;
  let view = null;
  const calls = { texts: [], fills: [], rounded: [], strokes: [], actions: [], sizes: [], corners: [], panels: [], backdrops: [] };
  let panel = { corner_radius: 0, shadow_radius: 0, offset_x: 0, offset_y: 0, color: 0 };
  const bounded = (value, minimum, maximum = 4096) => Number.isFinite(value) && value >= minimum && value <= maximum;
  const memory = () => new Uint8Array(instance.exports.memory.buffer);
  const read = (p, n) => new TextDecoder("utf-8", { fatal: true }).decode(memory().subarray(p, p + n));
  const get = (scope, p, n) => {
    let v = scope === 0 ? view : scope === 1 ? options : scope === 2 ? settings : undefined;
    const path = read(p, n);
    if (path === "") return v;
    for (const key of path.slice(1).split("/").map(k => k.replaceAll("~1", "/").replaceAll("~0", "~"))) v = v?.[key];
    return v;
  };
  ({ instance } = await WebAssembly.instantiate(bytes, {
    env: { abort: (_m, _f, line, column) => { throw new Error(`abort ${line}:${column}`); } },
    weasel: {
      data_kind: (s, p, n) => {
        const v = get(s, p, n);
        return v === undefined ? 0 : v === null ? 1 : typeof v === "boolean" ? 2 :
          typeof v === "number" || typeof v === "bigint" ? 3 : typeof v === "string" ? 4 : Array.isArray(v) ? 5 : 6;
      },
      data_len: (s, p, n) => {
        const v = get(s, p, n);
        return typeof v === "string" ? new TextEncoder().encode(v).length : Array.isArray(v) ? v.length : -1;
      },
      data_i64: (s, p, n) => BigInt.asIntN(64, BigInt(get(s, p, n) ?? 0)),
      data_number: (s, p, n) => Number(get(s, p, n)),
      data_string: (s, p, n, dst, cap) => {
        const v = get(s, p, n);
        if (typeof v !== "string") return -1;
        const b = new TextEncoder().encode(v);
        if (cap >= b.length) memory().set(b, dst);
        return b.length;
      },
      set_font: () => {},
      set_text_glow: () => {},
      line_height: (_slot, size) => size * 1.4,
      measure_text: (p, n, _font, size) => Array.from(read(p,n)).reduce((w,c) => w + (c.codePointAt(0) > 0x2e7f ? size : size * 0.55), 0),
      fill_rect: (x, y, w, h, color) => calls.fills.push({x,y,w,h,color: color >>> 0}),
      fill_rounded_rect: (x,y,w,h,radius,color) => calls.rounded.push({x,y,w,h,radius,color: color >>> 0}),
      stroke_rect: (x,y,w,h,color,width) => calls.strokes.push({x,y,w,h,color: color >>> 0,width}),
      draw_text: (p,n,x,y,font,size,color) => calls.texts.push({text:read(p,n),x,y,font,size,color: color >>> 0}),
      set_corner_radius: radius => {
        if (!bounded(radius, 0)) throw new Error("invalid window corner radius");
        panel = { ...panel, corner_radius: radius };
        calls.corners.push(radius);
      },
      set_panel: (radius, shadow_radius, offset_x, offset_y, color) => {
        if (!bounded(radius, 0) || !bounded(shadow_radius, 0, 250) ||
            !bounded(offset_x, -1024, 1024) || !bounded(offset_y, -1024, 1024)) throw new Error("invalid panel style");
        panel = { corner_radius: radius, shadow_radius, offset_x, offset_y, color: color >>> 0 };
        calls.panels.push({ ...panel });
      },
      set_size: (w,h) => calls.sizes.push({w,h}),
      set_backdrop: (enabled, tint, blur_sigma, backdrop_balance, afterglow_balance, color_balance, fallback_color) => {
        const weights = [backdrop_balance, afterglow_balance, color_balance];
        if (![0, 1].includes(enabled) || !bounded(blur_sigma, 0, 64) ||
            weights.some(v => !bounded(v, 0, 1)) || Math.abs(weights.reduce((a,b) => a+b, 0) - 1) > .001)
          throw new Error("invalid backdrop style");
        calls.backdrops.push({enabled, tint: tint >>> 0, blur_sigma, backdrop_balance,
          afterglow_balance, color_balance, fallback_color: fallback_color >>> 0});
      },
      send_action: (a,i) => calls.actions.push([a,i]),
      request_frame: () => {}, time_ms: () => Date.now(), log: () => {},
    }
  }));
  if (instance.exports.default_config) {
    const range = BigInt.asUintN(64, instance.exports.default_config());
    const defaults = JSON.parse(read(Number(range & 0xffffffffn), Number(range >> 32n)));
    const merge = (base, patch) => {
      if (base && patch && !Array.isArray(base) && !Array.isArray(patch)
          && typeof base === "object" && typeof patch === "object") {
        for (const [key, value] of Object.entries(patch)) base[key] = merge(base[key], value);
        return base;
      }
      return patch;
    };
    options = merge(defaults, options);
  }
  return { exports: instance.exports, calls, get panel_style() { return { ...panel }; }, render(next) { view = next; return instance.exports.render(); } };
}
