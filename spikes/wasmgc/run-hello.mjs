// hello.zu on the GC backend: strings on the GC heap, typed input through
// the payload buffer, dynamic attribute + text patches. Uses the real
// gcApp adapter and the real wire decoder.
//
//   cargo run -p zumar-wasmgc --bin zuc-gc -- examples/lang-hello/hello.zu \
//     -o spikes/wasmgc/hello-emitted.wasm
//   node spikes/wasmgc/run-hello.mjs

import { readFile } from "node:fs/promises";
import { decodeInit, decodeUpdate } from "../../www/zumar-wire.js";
import { gcApp } from "../../www/zumar-gc.js";

const bytes = await readFile(new URL("./hello-emitted.wasm", import.meta.url));
const { instance } = await WebAssembly.instantiate(bytes, {});
const app = gcApp(instance.exports);

const initMsg = decodeInit(app.init());
const fire = (path, name, value) =>
  decodeUpdate(app.dispatch(Uint32Array.from(path), name, value));
const textAt = (ps, path) =>
  ps.find((p) => p.op === "setText" && p.path.join(",") === path.join(","))?.text;
const attrAt = (ps, path, name) =>
  ps.find((p) => p.op === "setAttr" && p.name === name && p.path.join(",") === path.join(","))?.value;

let failures = 0;
const check = (label, got, want) => {
  const ok = got === want;
  if (!ok) failures++;
  console.log(`${ok ? "ok  " : "FAIL"} ${label}: ${JSON.stringify(got)}${ok ? "" : ` (want ${JSON.stringify(want)})`}`);
};

// View: div.hello > [h1, p.sub, input, p.greet, button, p.count]
check("greet when empty", initMsg.root.children[3].children[0].text, "nor zara?");
check("input value attr", initMsg.root.children[2].attrs.value, "");
check("events", initMsg.events.map((e) => e.name).join(","), "click,input");

// Type a name — including multi-byte UTF-8 through the payload buffer.
let r = fire([2], "input", "Eñaut");
check("greet after typing", textAt(r.patches, [3, 0]), "kaixo, Eñaut!");
check("value attr tracks model", attrAt(r.patches, [2], "value"), "Eñaut");

// Clear: greeting resets, value empties, tap counter shows (itoa + concat).
r = fire([4], "click");
check("greet after clear", textAt(r.patches, [3, 0]), "nor zara?");
check("value attr cleared", attrAt(r.patches, [2], "value"), "");
check("clears count", textAt(r.patches, [5, 0]), "1 clears");

// Again, via the button's bubbled text node.
r = fire([4, 0], "click");
check("second clear", textAt(r.patches, [5, 0]), "2 clears");

// String equality drives the greeting branch: type then erase.
fire([2], "input", "x");
r = fire([2], "input", "");
check("empty string == \"\" branch", textAt(r.patches, [3, 0]), "nor zara?");

console.log(
  failures === 0
    ? `\nPASS — GC-heap strings: typing, UTF-8 payloads, ++/==, dynamic attrs (${bytes.length}-byte module)`
    : `\n${failures} FAILURES`
);
process.exit(failures === 0 ? 0 : 1);
