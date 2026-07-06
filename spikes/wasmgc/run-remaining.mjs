// The three newly-unlocked GC apps in one harness:
// - queue.zu: Maybe via null refs (head + total case)
// - expenses.zu: fold (inline loop) + toInt (emitted atoi)
// - toggle.zu: onCheck / Bool payloads through dispatch's checked param
import { readFile } from "node:fs/promises";
import { decodeInit, decodeUpdate } from "../../www/zumar-wire.js";
import { gcApp } from "../../www/zumar-gc.js";

let fails = 0;
const check = (label, got, want) => {
  const ok = JSON.stringify(got) === JSON.stringify(want);
  if (!ok) fails++;
  console.log(`${ok ? "ok  " : "FAIL"} ${label}: ${JSON.stringify(got)}${ok ? "" : ` want ${JSON.stringify(want)}`}`);
};
const load = async (name) => {
  const bytes = await readFile(new URL(`./${name}-emitted.wasm`, import.meta.url));
  const { instance } = await WebAssembly.instantiate(bytes, {});
  const app = gcApp(instance.exports);
  return { app, init: decodeInit(app.init()) };
};
const textAt = (ps, path) =>
  ps.find((p) => p.op === "setText" && p.path.join(",") === path.join(","))?.text;

// --- queue: Maybe as null refs ------------------------------------------
{
  const { app, init } = await load("queue");
  const fire = (path, name, value) => decodeUpdate(app.dispatch(Uint32Array.from(path), name, value));
  // view: div.queue > [h1, div.next, form[input,btn], button.pop, div.count, ul]
  check("queue: empty head -> none arm", init.root.children[1].children[0].text, "nothing queued");
  fire([2, 0], "input", "build");
  let r = fire([2], "submit");
  check("queue: head after enqueue", textAt(r.patches, [1, 0]), "next up: build");
  fire([2, 0], "input", "test");
  fire([2], "submit");
  r = fire([3], "click"); // dequeue front (case head in the where clause)
  check("queue: dequeue advances head", textAt(r.patches, [1, 0]), "next up: test");
  r = fire([3], "click");
  check("queue: dequeue to empty", textAt(r.patches, [1, 0]), "nothing queued");
}

// --- expenses: fold + atoi ------------------------------------------------
{
  const { app, init } = await load("expenses");
  const fire = (path, name, value) => decodeUpdate(app.dispatch(Uint32Array.from(path), name, value));
  check("expenses: fold at init", init.root.children[3].children[0].text, "0¢ total");
  fire([2, 0], "input", "coffee");
  fire([2, 1], "input", "450");
  let r = fire([2], "submit");
  check("expenses: toInt + fold", textAt(r.patches, [3, 0]), "450¢ total");
  fire([2, 0], "input", "lunch");
  fire([2, 1], "input", "1200");
  r = fire([2], "submit");
  check("expenses: fold accumulates", textAt(r.patches, [3, 0]), "1650¢ total");
  fire([2, 0], "input", "junk");
  fire([2, 1], "input", "12x4");
  r = fire([2], "submit");
  check("expenses: atoi rejects junk (0)", textAt(r.patches, [3, 0]), "1650¢ total");
  fire([2, 0], "input", "refund");
  fire([2, 1], "input", "-150");
  r = fire([2], "submit");
  check("expenses: negative atoi", textAt(r.patches, [3, 0]), "1500¢ total");
}

// --- toggle: Bool payloads ------------------------------------------------
{
  const { app, init } = await load("toggle");
  const fire = (path, name, value, checked) =>
    decodeUpdate(app.dispatch(Uint32Array.from(path), name, value, checked));
  check("toggle: init label", init.root.children[1].children[0].text, "light");
  let r = fire([0], "change", "on", true);
  check("toggle: checked=true", textAt(r.patches, [1, 0]), "dark");
  r = fire([0], "change", "on", false);
  check("toggle: checked=false", textAt(r.patches, [1, 0]), "light");
}

console.log(fails === 0 ? "\nPASS — Maybe (null refs), fold+atoi, Bool payloads on the GC backend" : `\n${fails} FAILURES`);
process.exit(fails ? 1 : 0);
