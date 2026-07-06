// Drives the WasmGC spike: load the module, exercise it, and decode its
// output with the *real* framework decoder to prove the bytes are protocol
// -correct. Run: wasm-tools parse counter.wat -o counter.wasm && node run.mjs

import { readFile } from "node:fs/promises";
import { decodeInit, decodeUpdate } from "../../www/zumar-wire.js";

const bytes = await readFile(new URL("./counter.wasm", import.meta.url));
const { instance } = await WebAssembly.instantiate(bytes, {});
const { mem, init, dispatch } = instance.exports;

const read = (len) => new Uint8Array(mem.buffer.slice(0, len));

const initTree = decodeInit(read(init()));
console.log("init  ->", JSON.stringify(initTree.root));

for (const delta of [1, 1, 1]) {
  const up = decodeUpdate(read(dispatch(delta)));
  console.log(`+${delta}  ->`, JSON.stringify(up.patches));
}

// Sanity: the decoded shapes must match what the JS shim expects.
const ok =
  initTree.root.tag === "span" &&
  initTree.root.children[0].text === "0" &&
  decodeUpdate(read(dispatch(0))).patches[0].op === "setText";
console.log(ok ? "\nPASS — WasmGC module speaks the wire protocol" : "\nFAIL");
process.exit(ok ? 0 : 1);
