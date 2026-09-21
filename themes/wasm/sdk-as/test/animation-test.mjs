// SDK wrapper contract tests, not a simulation of the native scheduler/compositor.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
const calls = [];
const record = name => (...args) => calls.push([name, ...args]);
const { instance: { exports: sdk } } = await WebAssembly.instantiate(
  readFileSync(new URL("../build/animation-test.wasm", import.meta.url)), {
    env: { abort: (_m, _f, line, column) => { throw new Error(`assert ${line}:${column}`); } },
    weasel_v2: {
      request_wakeup: record("wake"), cancel_wakeup: record("cancel"), request_frame: record("frame"),
      layer_content: record("content"), layer_set: record("set"), layer_animate: record("animate"),
      layer_stop: record("stop"), layer_remove: record("remove"),
    },
  });
sdk.math();
sdk.schedule();
assert.deepEqual(calls.splice(0), [["wake", 100], ["wake", 200], ["cancel"], ["wake", 300], ["frame"]]);
sdk.layers();
assert.deepEqual(calls, [["content", 42, 100, 50], ["content", 0, 0, 0], ["set", 42, 0, 0.5],
  ["animate", 42, 0, 1, 200, 1], ["stop", 42, 0, 1], ["remove", 42]]);
console.log("Animation math, wakeup scheduling, and scoped layer wrappers passed");
