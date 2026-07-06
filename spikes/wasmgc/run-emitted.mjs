// Drives the zuc-gc-emitted module: counter.zu compiled straight to a
// WasmGC binary by wasm-encoder, no Rust toolchain in the loop. Decoded by
// the real framework decoder; behavior must match the Rust-backend build.
//
//   cargo run -p zumar-wasmgc --bin zuc-gc -- examples/lang-counter/counter.zu \
//     -o spikes/wasmgc/counter-emitted.wasm
//   node spikes/wasmgc/run-emitted.mjs

import { readFile } from "node:fs/promises";
import { decodeInit, decodeUpdate } from "../../www/zumar-wire.js";

const bytes = await readFile(new URL("./counter-emitted.wasm", import.meta.url));
const { instance } = await WebAssembly.instantiate(bytes, {});
const { mem, init, dispatch, path_buf } = instance.exports;

const read = (len) => new Uint8Array(mem.buffer.slice(0, len));
const initMsg = decodeInit(read(init()));

const eventIdx = (name) => initMsg.events.findIndex((e) => e.name === name);
function fire(path, event = "click") {
  new Uint32Array(mem.buffer, path_buf.value, path.length).set(path);
  return decodeUpdate(read(dispatch(eventIdx(event), path.length)));
}
const textAt = (patches, path) =>
  patches.find((p) => p.op === "setText" && p.path.join(",") === path.join(","))?.text;

let failures = 0;
function check(label, got, want) {
  const ok = got === want;
  if (!ok) failures++;
  console.log(`${ok ? "ok  " : "FAIL"} ${label}: ${JSON.stringify(got)}${ok ? "" : ` (want ${JSON.stringify(want)})`}`);
}

// Initial tree shape — root div.counter, count "0" at [2,1,0], note "" at [4,0].
check("root tag", initMsg.root.tag, "div");
check("root class", initMsg.root.attrs.class, "counter");
check("count text", initMsg.root.children[2].children[1].children[0].text, "0");
check("note text", initMsg.root.children[4].children[0].text, "");
check("events", initMsg.events.map((e) => e.name).join(","), "click");

// + button at [2,2]; a click on its text child [2,2,0] must bubble to it.
check("inc", textAt(fire([2, 2]).patches, [2, 1, 0]), "1");
check("inc via bubbled text node", textAt(fire([2, 2, 0]).patches, [2, 1, 0]), "2");

// - button at [2,0].
check("dec", textAt(fire([2, 0]).patches, [2, 1, 0]), "1");

// Reset at [3].
check("reset", textAt(fire([3]).patches, [2, 1, 0]), "0");

// The note flips at >9 (compile-time patch plan re-evaluates the if).
let last;
for (let i = 0; i < 10; i++) last = fire([2, 2]);
check("count at 10", textAt(last.patches, [2, 1, 0]), "10");
check("note at 10", textAt(last.patches, [4, 0]), "double digits!");

// And "very negative!" below -9 (negative itoa + the nested else-if).
fire([3]);
for (let i = 0; i < 10; i++) last = fire([2, 0]);
check("count at -10", textAt(last.patches, [2, 1, 0]), "-10");
check("note at -10", textAt(last.patches, [4, 0]), "very negative!");

// A click somewhere without a handler is an empty, well-formed update.
const noop = fire([0]);
check("no handler -> zero patches", noop.patches.length, 0);

console.log(
  failures === 0
    ? `\nPASS — counter.zu as a ${bytes.length}-byte self-contained WasmGC module, driven through the real wire decoder`
    : `\n${failures} FAILURES`
);
process.exit(failures === 0 ? 0 : 1);
