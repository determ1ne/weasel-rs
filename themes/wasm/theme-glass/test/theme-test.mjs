import { EventKind, FrameResult } from "../../sdk-as/test/abi.mjs";
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { createHost, createResourceHost, createViewHost } from '../../sdk-as/test/host.mjs';

const bytes = readFileSync(process.argv[2] ?? new URL('../target/wasm32-unknown-unknown/release/glass.wasm', import.meta.url));
const module = new WebAssembly.Module(bytes);
for (const entry of WebAssembly.Module.imports(module)) assert.equal(entry.module, 'weasel_v2');
let memory;
let view;
let calls = [];
let actions = [];
const decoder = new TextDecoder();
const encoder = new TextEncoder();
const string = (p, n) => decoder.decode(new Uint8Array(memory.buffer, p, n));
const value = (scope, p, n) => {
  assert.equal(scope, 0);
  return string(p, n).split('/').slice(1).reduce((v, key) => v?.[key], view);
};
const host = {
  ...createViewHost(() => view, () => new Uint8Array(memory.buffer)),
  ...createResourceHost(string, t => calls.push(["text",t.text,t.x,t.y,t.font,t.size,t.color,t.glow,t.glow_color])),
  set_text_glow(radius, color) { assert.equal(radius, 3); assert.equal(color >>> 0, 0xffffffff); },
  line_height(slot, size) { return size * 1.4; },
  data_kind(s, p, n) {
    const v = value(s, p, n);
    return v === undefined ? 0 : v === null ? 1 : typeof v === 'boolean' ? 2 :
      typeof v === 'number' ? 3 : typeof v === 'string' ? 4 : Array.isArray(v) ? 5 : 6;
  },
  data_len(s, p, n) { const v = value(s, p, n); return typeof v === 'string' ? encoder.encode(v).length : Array.isArray(v) ? v.length : -1; },
  data_i64(s, p, n) { return BigInt(value(s, p, n) ?? 0); },
  data_string(s, p, n, dst, capacity) {
    const bytes = encoder.encode(value(s, p, n));
    if (bytes.length <= capacity) new Uint8Array(memory.buffer, dst, bytes.length).set(bytes);
    return bytes.length;
  },
  measure_text(p, n, font, size) { return [...string(p, n)].length * size / 2; },
  draw_text(p, n, ...args) { calls.push(['text', string(p, n), ...args]); },
  fill_rounded_rect(...args) { calls.push(['rounded', ...args]); },
  set_panel(...args) { calls.push(['panel', ...args]); },
  set_backdrop(...args) { calls.push(['backdrop', ...args]); },
  set_size(...args) { calls.push(['size', ...args]); },
  send_action(...args) { actions.push(args); },
};
const instance = new WebAssembly.Instance(module, { weasel_v2: host });
const event = (kind, detail=0, x=0, y=0, now=0) => {
  const result=instance.exports.theme_event(kind,detail,x,y,now);
  return result===FrameResult.Keep || result===FrameResult.Present ? 0 : result;
};
const e = { ...instance.exports, init: instance.exports.theme_create,
  render: () => event(EventKind.View), mouse: (kind,x,y) => event(EventKind.Pointer,kind,x,y),
  frame: now => event(EventKind.Animation,0,0,0,now), hide: () => event(EventKind.Hide), refresh: dark => event(EventKind.Appearance,dark) };
memory = e.memory;
assert.equal(e.theme_abi_version(), 2);
assert.equal(e.init(0, 0), 0);
assert.equal(e.theme_capabilities(), 0);
const snapshot = () => ({ items: [
  { primary_text: '你好😀', secondary_text: 'nǐ hǎo', enabled: true },
  { primary_text: 'disabled', secondary_text: '', enabled: false },
  { primary_text: 'third', secondary_text: '', enabled: true },
], selected_index: 0 });
view = snapshot();
assert.equal(e.render(), 0);
assert(calls.some(c => c[0] === 'text' && c[1] === '你好😀'));
const backdrop = calls.find(c => c[0] === 'backdrop').slice(1);
assert.deepEqual(backdrop, [1, 0xffdce8f0 | 0, 19, Math.fround(.30), Math.fround(.30), Math.fround(.40), 0xffdce8f0 | 0]);
assert.deepEqual(calls.find(c => c[0] === 'panel').slice(1), [10, 10, 0, 2, 0x50002040]);
assert.equal(calls.filter(c => c[0] === 'rounded').length, 0);
const firstFrame = calls;
const label2 = calls.find(c => c[0] === 'text' && c[1] === '2');
const label3 = calls.find(c => c[0] === 'text' && c[1] === '3');
assert.equal(label2[3], label3[3]);
assert.ok(label3[2] > label2[2]);
assert.equal(calls.find(c => c[0] === 'size')[2], 38);
const candidate = calls.find(c => c[0] === 'text' && c[1] === '你好😀');
assert.equal(candidate[5], 14);
assert.equal(candidate[4], 4); // selected primary text uses the bold font slot
assert.equal(candidate[3], label2[3]);
assert.ok(Math.abs(candidate[3] - (4 + (30 - 14 * 1.4) / 2)) < 0.001);
calls = [];
e.refresh(1);
assert.deepEqual(calls, firstFrame);
e.mouse(2, 20, 20); // release without press
assert.equal(actions.length, 0);
e.mouse(0, 20, 20); e.mouse(2, 20, 20);
assert.deepEqual(actions, [[0, 0]]);
actions = [];
e.mouse(0, label2[2], 20); e.mouse(2, label2[2], 20); // disabled
e.mouse(0, 20, 20); e.mouse(2, label3[2], 20); // different item
e.mouse(0, 20, 20); e.mouse(3, 20, 20); e.mouse(2, 20, 20);
e.mouse(0, 20, 20); e.render(); e.mouse(2, 20, 20);
e.mouse(0, 20, 20); e.hide(); e.mouse(2, 20, 20);
assert.deepEqual(actions, []);
e.render();
for (const [x, y] of [[0, 20], [20, 110], [NaN, 20], [20, Infinity]]) {
  e.mouse(0, x, y); e.mouse(2, x, y);
}
assert.deepEqual(actions, []);
e.mouse(0, label3[2], 20); e.mouse(2, label3[2], 20);
assert.deepEqual(actions, [[0, 2]]);
view.preedit = { text: 'abc', cursor: 1 };
assert.notEqual(e.render(), 0);
actions = []; e.mouse(0, 20, 20); e.mouse(2, 20, 20);
assert.deepEqual(actions, []);
view = { items: [] }; assert.equal(e.render(), 0);
view = null; assert.notEqual(e.render(), 0);
view = { items: Array(65).fill(snapshot().items[0]) }; assert.notEqual(e.render(), 0);
view = { items: [{ ...snapshot().items[0], primary_text: 'x'.repeat(1000) }] }; assert.notEqual(e.render(), 0);
e.frame(0);
// Also exercise the shared fixture's host-side parameter validation.
const shared = await createHost(bytes);
assert.equal(shared.exports.init(0, 1), 0);
assert.equal(shared.render(snapshot()), 0);
assert.deepEqual(shared.calls.backdrops, [{
  enabled: 1, tint: 0xffdce8f0, blur_sigma: 19,
  backdrop_balance: Math.fround(.30), afterglow_balance: Math.fround(.30),
  color_balance: Math.fround(.40), fallback_color: 0xffdce8f0,
}]);
assert.deepEqual(shared.calls.fills, []);
assert.equal(shared.calls.rounded.length, 0);
shared.exports.mouse(0, 20, 20); shared.exports.mouse(2, 20, 20);
assert.deepEqual(shared.calls.actions, [[0, 0]]);
console.log('glass WASM: ABI, backdrop, transparency, Unicode, clicks, lifecycle, preedit rejection and bounds passed');
