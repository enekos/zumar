// kanban.zu on the GC backend: user-defined enums as i32 tags, case chains
// in every value position (record update, class attr, glyph text, filter
// condition inside the region's comprehension), enum == as i32 compares.
//
//   cargo run -p zumar-wasmgc --bin zuc-gc -- examples/lang-kanban/kanban.zu \
//     -o spikes/wasmgc/kanban-emitted.wasm
//   node spikes/wasmgc/run-kanban.mjs

import { readFile } from "node:fs/promises";
import { decodeInit, decodeUpdate } from "../../www/zumar-wire.js";
import { gcApp } from "../../www/zumar-gc.js";

const bytes = await readFile(new URL("./kanban-emitted.wasm", import.meta.url));
const { instance } = await WebAssembly.instantiate(bytes, {});
const app = gcApp(instance.exports);

const initMsg = decodeInit(app.init());
const fire = (path, name, value) =>
  decodeUpdate(app.dispatch(Uint32Array.from(path), name, value));
const textAt = (ps, path) =>
  ps.find((p) => p.op === "setText" && p.path.join(",") === path.join(","))?.text;
const regionAt = (ps, path) =>
  ps.find((p) => p.op === "replace" && p.path.join(",") === path.join(","))?.node;
const attrAt = (ps, path, name) =>
  ps.find((p) => p.op === "setAttr" && p.name === name && p.path.join(",") === path.join(","))?.value;

// View: div.kanban > [h1, p.sub, form[input,btn], div.counts[text],
//                     div.filters[all,active,finished], ul(region)]
const COUNTS = [3, 0];
const UL = [5];
const liGlyph = (ul, k) => ul.children[k].children[0].children[0].text;
const liName = (ul, k) => ul.children[k].children[1].children[0].text;
const liClass = (ul, k) => ul.children[k].attrs.class;

let fails = 0;
const check = (label, got, want) => {
  const ok = JSON.stringify(got) === JSON.stringify(want);
  if (!ok) fails++;
  console.log(`${ok ? "ok  " : "FAIL"} ${label}: ${JSON.stringify(got)}${ok ? "" : ` want ${JSON.stringify(want)}`}`);
};

check("counts at init", initMsg.root.children[3].children[0].text, "todo 0 / doing 0 / done 0");
check("all-filter starts on", initMsg.root.children[4].children[0].attrs.class, "on");

// Add two tasks.
fire([2, 0], "input", "spec");
let r = fire([2], "submit");
fire([2, 0], "input", "build");
r = fire([2], "submit");
let ul = regionAt(r.patches, UL);
check("two tasks", ul.children.length, 2);
check("fresh tasks are todo", liClass(ul, 0) + "," + liGlyph(ul, 0), "todo,[ ]");
check("counts", textAt(r.patches, COUNTS), "todo 2 / doing 0 / done 0");

// Advance "spec" twice around the board: Todo -> Doing -> Done.
r = fire([5, 0, 1], "click");
ul = regionAt(r.patches, UL);
check("spec doing", liClass(ul, 0) + "," + liGlyph(ul, 0), "doing,[~]");
check("counts after advance", textAt(r.patches, COUNTS), "todo 1 / doing 1 / done 0");
r = fire([5, 0, 1], "click");
ul = regionAt(r.patches, UL);
check("spec done", liClass(ul, 0) + "," + liGlyph(ul, 0), "done,[x]");

// Filter: finished shows only spec; active shows only build.
r = fire([4, 2], "click");
ul = regionAt(r.patches, UL);
check("finished filter", ul.children.length + ":" + liName(ul, 0), "1:spec");
check("finished button lit", attrAt(r.patches, [4, 2], "class"), "on");
check("all button unlit", attrAt(r.patches, [4, 0], "class"), "");
r = fire([4, 1], "click");
ul = regionAt(r.patches, UL);
check("active filter", ul.children.length + ":" + liName(ul, 0), "1:build");

// Advance through a filtered region: item index 0 is "build" here.
r = fire([5, 0, 1], "click");
ul = regionAt(r.patches, UL);
check("advance through filter", liClass(ul, 0), "doing");

// Back to all: both tasks, cycled statuses hold.
r = fire([4, 0], "click");
ul = regionAt(r.patches, UL);
check("all again", ul.children.length, 2);
check("statuses held", liClass(ul, 0) + "," + liClass(ul, 1), "done,doing");

console.log(
  fails === 0
    ? `\nPASS — enums + total case on the GC backend (${bytes.length}-byte module)`
    : `\n${fails} FAILURES`
);
process.exit(fails ? 1 : 0);
