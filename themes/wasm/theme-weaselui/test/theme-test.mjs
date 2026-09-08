import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createHost } from "../../sdk-as/test/host.mjs";

// Host overlays a partial patch over the module-embedded defaults.
const host = await createHost(readFileSync(new URL("../build/weaselui.wasm", import.meta.url)), {color: {hilited_candidate_back: "#123456"}});
const {exports: theme, calls} = host;
assert.equal(theme.abi_version(), 1);
assert.equal(theme.probe_preedit(1), 0);
assert.equal(theme.init(0, 1), 0);
assert.equal(host.render({content_id:1,visible:true,preedit:null,anchor:null,
  items:[{primary_text:"输入法",secondary_text:"shu ru fa",enabled:true},
    {primary_text:"输入",secondary_text:"",enabled:true}],
  selected_index:0,page_start:0,total_item_count:null,can_page_previous:false,can_page_next:true}),0);
assert.ok(Math.abs(calls.sizes.at(-1).h - (24 + 2 * (14 * 4 / 3 * 1.4 + 4) + 5)) < 0.01);
assert.ok(calls.texts.some(c => c.text === "1."));
assert.equal(calls.fills.length, 0);
assert.deepEqual(calls.corners, []);
assert.deepEqual(calls.panels, [{corner_radius: 4, shadow_radius: 0, offset_x: 4, offset_y: 4, color: 0x40000000}]);
assert.equal(calls.rounded.length, 3);
assert.equal(calls.rounded[2].color, 0xff123456);
assert.equal(calls.rounded[2].radius, 4);
assert.ok(calls.texts.some(c => c.text === "输入法" && c.color === 0xffffffff && Math.abs(c.size - 14 * 4 / 3) < 0.01));
assert.ok(calls.texts.some(c => c.text === "输入" && c.color === 0xff000000));
assert.equal(calls.rounded[0].color, 0xffe0e0e0);
assert.equal(calls.rounded[1].color, 0xffeeeeec);
theme.mouse(0, 20, 60);
assert.deepEqual(calls.actions, []);
theme.mouse(2, 20, 60);
assert.deepEqual(calls.actions, [[0,1]]);
calls.actions.length = 0;
theme.mouse(0,20,60); theme.mouse(3,0,0); theme.mouse(2,20,60);
assert.deepEqual(calls.actions, []);
theme.hide();
// One matrix covers layout, global mode selection and the shifted hit rectangles.
for (const horizontal of [false, true]) for (const preedit_type of ["composition", "preview"]) {
  const h = await createHost(readFileSync(new URL("../build/weaselui.wasm", import.meta.url)),
    {horizontal}, {preedit_type});
  h.exports.init(0, 0);
  const snapshot = {content_id: 2, visible: true, preedit: {text: "shu ru fa", cursor: 9},
    items: [{primary_text: "输入法", secondary_text: "(rime)", enabled: true},
      {primary_text: "输入", secondary_text: "", enabled: true}], selected_index: 0};
  assert.equal(h.render(snapshot), 0);
  const preedit = h.calls.texts[0];
  assert.equal(preedit.text, preedit_type === "composition" ? "shu ru fa" : "输入法");
  const first = h.calls.texts.find(t => t.text === "1.");
  const second = h.calls.texts.find(t => t.text === "2.");
  assert.ok(first.y > preedit.y);
  if (horizontal) { assert.equal(first.y, second.y); assert.ok(second.x > first.x); }
  else { assert.equal(first.x, second.x); assert.ok(second.y > first.y); }
  h.exports.mouse(0, second.x + 1, second.y + 1);
  h.exports.mouse(2, second.x + 1, second.y + 1);
  assert.deepEqual(h.calls.actions, [[0, 1]]);
  if (preedit_type === "composition") {
    h.calls.texts.length = 0;
    h.render({...snapshot, preedit: {text: "abccd", cursor: 4}});
    const [before, marker, after] = h.calls.texts;
    assert.deepEqual([before.text, marker.text, after.text], ["abcc", "^", "d"]);
    assert.ok(marker.y > before.y);
    assert.equal(after.y, before.y);
    assert.ok(after.x >= marker.x + marker.size * 0.55 - 0.001);
    assert.equal(marker.color, before.color);
  }
  h.calls.texts.length = 0;
  h.render({...snapshot, preedit: null});
  assert.equal(h.calls.texts[0].text, "1.");
}
console.log("WeaselUI: colors, horizontal/vertical preedit and click cancellation passed");
