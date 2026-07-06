// todo.zu on the GC backend: records as GC structs, a `for` region over the
// list, comprehensions in updates, and parameterized handlers — Toggle(t.id)
// resolves the clicked item from the event path at dispatch time.
//
//   cargo run -p zumar-wasmgc --bin zuc-gc -- examples/lang-todo/todo.zu \
//     -o spikes/wasmgc/todo-emitted.wasm
//   node spikes/wasmgc/run-todo.mjs

import { readFile } from "node:fs/promises";
import { decodeInit, decodeUpdate } from "../../www/zumar-wire.js";
import { gcApp } from "../../www/zumar-gc.js";

const bytes = await readFile(new URL("./todo-emitted.wasm", import.meta.url));
const { instance } = await WebAssembly.instantiate(bytes, {});
const app = gcApp(instance.exports);

const initMsg = decodeInit(app.init());
const fire = (path, name, value) =>
  decodeUpdate(app.dispatch(Uint32Array.from(path), name, value));
const textAt = (ps, path) =>
  ps.find((p) => p.op === "setText" && p.path.join(",") === path.join(","))?.text;
const attrAt = (ps, path, name) =>
  ps.find((p) => p.op === "setAttr" && p.name === name && p.path.join(",") === path.join(","))?.value;
const regionAt = (ps, path) =>
  ps.find((p) => p.op === "replace" && p.path.join(",") === path.join(","))?.node;

// View: div.todo > [h1, p.sub, form[input,button], div.bar[span,button], ul(region)]
const UL = [4];
const OPEN = [3, 0, 0];
const VALUE = [2, 0];
const itemLabel = (ul, k) => ul.children[k].children[1].children[0].text;
const itemCheck = (ul, k) => ul.children[k].children[0].children[0].text;
const itemClass = (ul, k) => ul.children[k].attrs.class;

let failures = 0;
const check = (label, got, want) => {
  const ok = got === want;
  if (!ok) failures++;
  console.log(`${ok ? "ok  " : "FAIL"} ${label}: ${JSON.stringify(got)}${ok ? "" : ` (want ${JSON.stringify(want)})`}`);
};

check("empty list at init", initMsg.root.children[4].children.length, 0);
check("open count at init", initMsg.root.children[3].children[0].children[0].text, "0 open");

// Add "milk": type into the input, submit the form.
fire(VALUE, "input", "milk");
let r = fire([2], "submit");
let ul = regionAt(r.patches, UL);
check("one item after add", ul.children.length, 1);
check("label", itemLabel(ul, 0), "milk");
check("open count", textAt(r.patches, OPEN), "1 open");
check("draft cleared", attrAt(r.patches, VALUE, "value"), "");

// Add "eggs".
fire(VALUE, "input", "eggs");
r = fire([2], "submit");
ul = regionAt(r.patches, UL);
check("two items", ul.children.length, 2);
check("second label", itemLabel(ul, 1), "eggs");

// Toggle the first item via its check span — the parameterized handler
// resolves items[0].id from the path.
r = fire([4, 0, 0], "click");
ul = regionAt(r.patches, UL);
check("toggled class", itemClass(ul, 0), "done");
check("toggled check", itemCheck(ul, 0), "[x]");
check("open count after toggle", textAt(r.patches, OPEN), "1 open");

// Reverse: eggs first now.
r = fire([3, 1], "click");
ul = regionAt(r.patches, UL);
check("reversed head", itemLabel(ul, 0), "eggs");
check("reversed tail is done", itemClass(ul, 1), "done");

// Delete eggs via its x button (first item after reverse).
r = fire([4, 0, 2], "click");
ul = regionAt(r.patches, UL);
check("one item after delete", ul.children.length, 1);
check("milk remains", itemLabel(ul, 0), "milk");

// Un-toggle milk via a click bubbled from the check's TEXT node.
r = fire([4, 0, 0, 0], "click");
ul = regionAt(r.patches, UL);
check("bubbled toggle", itemClass(ul, 0), "open");
check("open count restored", textAt(r.patches, OPEN), "1 open");

// Out-of-range item index (stale path) is a clean no-op.
r = fire([4, 7, 0], "click");
check("stale index -> no patches", r.patches.length, 0);

console.log(
  failures === 0
    ? `\nPASS — todo.zu on the GC backend: records, regions, comprehensions, parameterized handlers (${bytes.length}-byte module)`
    : `\n${failures} FAILURES`
);
process.exit(failures === 0 ? 0 : 1);
