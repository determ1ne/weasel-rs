import { EventKind, FrameResult, ViewField, ViewStringField, ConfigScope,DataKind,ResourceMetric } from "./abi.mjs";
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
    let v = scope === ConfigScope.Module ? options : scope === ConfigScope.Global ? settings : undefined;
    const path = read(p, n);
    if (path === "") return v;
    for (const key of path.slice(1).split("/").map(k => k.replaceAll("~1", "/").replaceAll("~0", "~"))) v = v?.[key];
    return v;
  };
  ({ instance } = await WebAssembly.instantiate(bytes, {
    env: { abort: (_m, _f, line, column) => { throw new Error(`abort ${line}:${column}`); } },
    weasel_v2: {
      ...createResourceHost(read, text => calls.texts.push(text)),
      ...createViewHost(() => view, memory),
      set_visible: () => {}, set_fixed_position: () => {}, begin_drag: () => {},
      data_kind: (s, p, n) => {
        const v = get(s, p, n);
        return v === undefined ? DataKind.Missing : v === null ? DataKind.Null : typeof v === "boolean" ? DataKind.Bool :
          typeof v === "number" || typeof v === "bigint" ? DataKind.Number : typeof v === "string" ? DataKind.String : Array.isArray(v) ? DataKind.Array : DataKind.Object;
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
      fill_rect: (x, y, w, h, color) => calls.fills.push({x,y,w,h,color: color >>> 0}),
      fill_rounded_rect: (x,y,w,h,radius,color) => calls.rounded.push({x,y,w,h,radius,color: color >>> 0}),
      stroke_rect: (x,y,w,h,color,width) => calls.strokes.push({x,y,w,h,color: color >>> 0,width}),
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
      request_frame: () => {}, time_ms: () => performance.now(), log: () => {}, report_notice: () => {},
    }
  }));
  const sections = WebAssembly.Module.customSections(new WebAssembly.Module(bytes), "weasel.settings");
  if (sections.length > 1) throw new Error("duplicate weasel.settings");
  if (sections.length) {
    const defaults = JSON.parse(new TextDecoder("utf-8", {fatal:true}).decode(sections[0])).defaults;
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
  const event = (kind, detail=0, x=0, y=0, now=0) => {
    const outcome = instance.exports.theme_event(kind,detail,x,y,now);
    // 既有主题行为测试关注成功/失败；真实Keep/Present事务由原生测试覆盖。
    return outcome===FrameResult.Keep || outcome===FrameResult.Present ? 0 : outcome;
  };
  const exports = { ...instance.exports, abi_version: () => instance.exports.theme_abi_version(), probe_preedit: required => !required || (instance.exports.theme_capabilities() & 1) ? 0 : 2, init: instance.exports.theme_create,
    mouse: (kind,x,y) => event(EventKind.Pointer,kind,x,y), frame: now => event(EventKind.Animation,0,0,0,now), hide: () => event(EventKind.Hide), refresh: dark => event(EventKind.Appearance,dark) };
  return { exports, calls, get panel_style() { return { ...panel }; }, render(next) { view = next; return event(EventKind.View); } };
}
/** 仅用于WASM逻辑测试的近似排版；实际字体、资源限额和事务由原生测试覆盖。 */
export function createResourceHost(read, draw) {
  let next=0;
  const resources=new Map();
  const put=value=>{resources.set(++next,value);return next;};
  return {
    font_create(p,n,size,weight) {
      const family=read(p,n);
      return put({size,slot:weight===700?4:family==="Segoe UI"?1:0});
    },
    text_layout_create(font,p,n) {
      const f=resources.get(font), text=read(p,n);
      return put({f,text,width:[...text].reduce((w,c)=>w+(c.codePointAt(0)>0x2e7f?f.size:f.size*.55),0)});
    },
    resource_release(id) {return resources.delete(id)?0:-3;},
    resource_metric(id,field) {
      const t=resources.get(id);
      return field===ResourceMetric.Width?t.width:field===ResourceMetric.Height?t.f.size*1.4:t.f.size;
    },
    draw_layout(id,x,y,color,glow,glow_color) {
      const t=resources.get(id);
      draw({text:t.text,x,y,font:t.f.slot,size:t.f.size,color:color>>>0,glow,glow_color});
    },
  };
}
/** 与原生view字段约定一致，仅用于guest逻辑测试。 */
export function createViewHost(snapshot, memory) {
  return {
    view_i64(field,index) {
      const v=snapshot();
      if (!v) return field===ViewField.AsciiMode||field===ViewField.TotalItemCount ? -1n : 0n;
      const values={
        [ViewField.ContentId]:v.content_id??0,
        [ViewField.Active]:v.active??false,
        [ViewField.Visible]:v.visible??false,
        [ViewField.AsciiMode]:v.ascii_mode??-1,
        [ViewField.ItemCount]:v.items?.length??0,
        [ViewField.SelectedIndex]:v.selected_index??0,
        [ViewField.PageStart]:v.page_start??0,
        [ViewField.TotalItemCount]:v.total_item_count??-1,
        [ViewField.CanPagePrevious]:v.can_page_previous??false,
        [ViewField.CanPageNext]:v.can_page_next??false,
        [ViewField.HasPreedit]:!!v.preedit,
        [ViewField.CursorUtf16]:v.preedit?.cursor??0,
        [ViewField.HasSnapshot]:1,
        [ViewField.ItemEnabled]:v.items?.[index]?.enabled??false,
        [ViewField.AnchorValid]:v.anchor?.valid??false,
        [ViewField.AnchorLeft]:v.anchor?.left??0,
        [ViewField.AnchorTop]:v.anchor?.top??0,
        [ViewField.AnchorRight]:v.anchor?.right??0,
        [ViewField.AnchorBottom]:v.anchor?.bottom??0,
      };
      if (!(field in values)) throw new Error("invalid field");
      return BigInt.asIntN(64,BigInt(values[field]));
    },
    view_string(field,index,dst,cap) {
      const v=snapshot();
      const text=field===ViewStringField.Preedit?v?.preedit?.text:field===ViewStringField.Primary?v?.items?.[index]?.primary_text:v?.items?.[index]?.secondary_text;
      if (text==null) return -1;
      const bytes=new TextEncoder().encode(text);
      if (cap>=bytes.length) memory().set(bytes,dst);
      return bytes.length;
    },
  };
}
