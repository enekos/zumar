// clock.zu on the GC backend: effects without a runtime. Command ids encode
// their compile-time callsite (id = counter*N + site); sub ids are site
// indices with a wanted-vs-active bitmask emitting start/stop deltas.
//
//   cargo run -p zumar-wasmgc --bin zuc-gc -- examples/lang-clock/clock.zu \
//     -o spikes/wasmgc/clock-emitted.wasm
//   node spikes/wasmgc/run-clock.mjs

import { readFile } from "node:fs/promises";
import { decodeInit, decodeUpdate } from "../../www/zumar-wire.js";
import { gcApp } from "../../www/zumar-gc.js";

const bytes = await readFile(new URL("./clock-emitted.wasm", import.meta.url));
const { instance } = await WebAssembly.instantiate(bytes, {});
const app = gcApp(instance.exports);

const click = (path) => decodeUpdate(app.dispatch(Uint32Array.from(path), "click"));
const textAt = (ps, path) =>
  ps.find((p) => p.op === "setText" && p.path.join(",") === path.join(","))?.text;

let fails = 0;
const check = (label, got, want) => {
  const ok = JSON.stringify(got) === JSON.stringify(want);
  if (!ok) fails++;
  console.log(`${ok ? "ok  " : "FAIL"} ${label}: ${JSON.stringify(got)}${ok ? "" : ` want ${JSON.stringify(want)}`}`);
};

// View: div.clock > [h1, p.sub, div.sec, button, blockquote, div.row[refetch,ping,toast]]
const initMsg = decodeInit(app.init());
check("init cmd", initMsg.cmds[0].spec, { kind: "httpGet", url: "./quote.txt" });
check("init sub start", initMsg.subs[0], { op: "start", id: 0, spec: { kind: "every", ms: 1000 } });

// Boot fetch resolves -> quote lands.
let r = decodeUpdate(app.resolve(initMsg.cmds[0].id, true, 200, "aupa"));
check("quote", textAt(r.patches, [4, 0]), "aupa");

// Clock tick at epoch 75000ms -> "15s".
r = decodeUpdate(app.notify(0, 75000));
check("seconds", textAt(r.patches, [2, 0]), "15s");

// Stop the clock -> stop delta; start again -> start delta, same id.
r = click([3]);
check("stop delta", r.subs, [{ op: "stop", id: 0 }]);
r = click([3]);
check("restart delta", r.subs, [{ op: "start", id: 0, spec: { kind: "every", ms: 1000 } }]);

// Refetch: a fresh command with a distinct id; failure -> "error 404".
r = click([5, 0]);
const refetchId = r.cmds[0].id;
check("refetch cmd", r.cmds[0].spec, { kind: "httpGet", url: "./quote.txt" });
check("ids are distinct", refetchId !== initMsg.cmds[0].id, true);
r = decodeUpdate(app.resolve(refetchId, false, 404, "nope"));
check("http error text", textAt(r.patches, [4, 0]), "error 404");

// Ping -> delay cmd -> pong.
r = click([5, 1]);
check("delay cmd", r.cmds[0].spec, { kind: "delay", ms: 1500 });
r = decodeUpdate(app.resolve(r.cmds[0].id, undefined, undefined, undefined));
check("pong", textAt(r.patches, [5, 2, 0]), "pong! (1.5s later)");

// Unknown notify id is a clean no-op.
r = decodeUpdate(app.notify(9, 0));
check("unknown sub id -> no patches", r.patches.length, 0);

console.log(
  fails === 0
    ? `\nPASS — effects on the GC backend: callsite-id commands, bitmask sub deltas, http error path (${bytes.length}-byte module)`
    : `\n${fails} FAILURES`
);
process.exit(fails ? 1 : 0);
