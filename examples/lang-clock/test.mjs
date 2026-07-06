// Effects-from-.zu harness, run against the Rust-backend build:
//   zuc build clock.zu --out app --zumar <repo>
//   wasm-pack build app --target web --out-dir ../www/pkg --release
//   node test.mjs   (from examples/lang-clock/www)
import { readFile } from "node:fs/promises";
import { decodeInit, decodeUpdate } from "./zumar-wire.js";

const mod = await import("./pkg/clock.js");
const bytes = await readFile(new URL("./pkg/clock_bg.wasm", import.meta.url));
await mod.default({ module_or_path: bytes });
const app = new mod.App();
const click = (path) =>
  decodeUpdate(app.dispatch(Uint32Array.from(path), "click", undefined, undefined, undefined));

let fails = 0;
const check = (label, got, want) => {
  const ok = JSON.stringify(got) === JSON.stringify(want);
  if (!ok) fails++;
  console.log(`${ok ? "ok  " : "FAIL"} ${label}: ${JSON.stringify(got)}${ok ? "" : ` want ${JSON.stringify(want)}`}`);
};

const init = decodeInit(app.init());
check("init cmd", init.cmds[0].spec, { kind: "httpGet", url: "./quote.txt" });
check("init sub", init.subs[0].spec, { kind: "every", ms: 1000 });
const clockSubId = init.subs[0].id;

let r = decodeUpdate(app.resolve(init.cmds[0].id, true, 200, "aupa"));
check("quote after fetch", r.patches.find((p) => p.op === "setText" && p.path.join() === "4,0")?.text, "aupa");

r = decodeUpdate(app.notify(clockSubId, 75000));
check("seconds text", r.patches.find((p) => p.op === "setText")?.text, "15s");

r = click([3]);
check("stop delta", r.subs[0].op, "stop");
r = click([3]);
check("restart delta", r.subs[0].op, "start");

r = click([5, 0]);
r = decodeUpdate(app.resolve(r.cmds[0].id, false, 404, "nope"));
check("http error text", r.patches.find((p) => p.op === "setText" && p.path.join() === "4,0")?.text, "error 404");

r = click([5, 1]);
check("delay cmd", r.cmds[0].spec, { kind: "delay", ms: 1500 });
r = decodeUpdate(app.resolve(r.cmds[0].id, undefined, undefined, undefined));
check("pong", r.patches.find((p) => p.op === "setText")?.text, "pong! (1.5s later)");

console.log(fails === 0 ? "\nPASS — effects from .zu" : `\n${fails} FAILURES`);
process.exit(fails ? 1 : 0);
