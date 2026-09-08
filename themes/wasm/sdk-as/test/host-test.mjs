// SDK example using the same in-memory fixture as external themes.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createHost } from "./host.mjs";
const host = await createHost(readFileSync(new URL("../build/release.wasm", import.meta.url)), {fontSize:20});
const {exports: theme, calls} = host;
assert.equal(theme.abi_version(), 1);
assert.equal(theme.init(0,1), 0);
assert.equal(host.render({
  content_id:18446744073709551615n,visible:true,preedit:null,anchor:null,
  items:[{primary_text:"你好😀",secondary_text:"ni hao",enabled:true}],
  selected_index:0,page_start:0,total_item_count:null,can_page_previous:false,can_page_next:false
}),0);
assert.ok(calls.texts.some(v => v.text === "你好😀" && v.size === 20));
assert.equal(calls.sizes.at(-1).h,42);
theme.mouse(0,10,21);
assert.deepEqual(calls.actions,[]);
theme.mouse(2,10,21);
assert.deepEqual(calls.actions,[[0,0]]);
calls.actions.length=0;
theme.mouse(0,10,21); theme.mouse(3,0,0); theme.mouse(2,10,21);
assert.deepEqual(calls.actions,[]);
theme.hide();
console.log("AssemblyScript SDK: typed view/config and click cancellation passed");
