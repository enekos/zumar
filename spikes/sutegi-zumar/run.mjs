// E2E harness for the live-mode P0 spike. Builds and spawns the sutegi
// server, then drives it through the REAL client stack: zumar-live.js's
// connect() over Node's WebSocket, updates decoded by zumar-wire.js — the
// same bytes a browser would see. Checks: initial render, click round-trips
// (+/-/reset), the conditional note, empty updates for listener-less paths,
// and per-connection state isolation (two sockets, independent counts).
//
//   node run.mjs

import { spawn, spawnSync } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const { connect } = await import(join(here, "../../www/zumar-live.js"));
const { decodeInit, decodeUpdate } = await import(join(here, "../../www/zumar-wire.js"));

const PORT = process.env.PORT || "8791";
const BASE = `http://127.0.0.1:${PORT}`;

let passed = 0;
let failed = 0;
const check = (name, cond) => {
  if (cond) passed++;
  else {
    failed++;
    console.error(`FAIL ${name}`);
  }
  console.log(`${cond ? "ok" : "FAIL"} - ${name}`);
};

// --- build + spawn -------------------------------------------------------

const build = spawnSync("cargo", ["build", "-q"], { cwd: here, stdio: "inherit" });
if (build.status !== 0) process.exit(1);

const server = spawn(join(here, "target/debug/sutegi-zumar"), [], {
  env: { ...process.env, HOST: "127.0.0.1", PORT },
  stdio: "ignore",
});
const stop = () => server.kill("SIGTERM");
process.on("exit", stop);

for (let i = 0; ; i++) {
  try {
    const r = await fetch(BASE + "/");
    if (r.ok) break;
  } catch {}
  if (i > 100) {
    console.error("server never came up");
    process.exit(1);
  }
  await new Promise((r) => setTimeout(r, 50));
}

// --- helpers -------------------------------------------------------------

// A live session: the real adapter plus a queue of decoded updates.
async function session() {
  const app = await connect(`ws://127.0.0.1:${PORT}/live`);
  const queue = [];
  let wake = null;
  app.onUpdate((bytes) => {
    queue.push(decodeUpdate(bytes));
    if (wake) wake();
  });
  const next = async () => {
    if (!queue.length) {
      await new Promise((r, j) => {
        wake = r;
        setTimeout(() => j(new Error("no update within 2s")), 2000);
      });
      wake = null;
    }
    return queue.shift();
  };
  const click = async (path) => {
    app.dispatch(Uint32Array.from(path), "click", undefined, undefined, undefined);
    return next();
  };
  return { app, click, next };
}

const nodeAt = (root, path) => path.reduce((n, i) => n.children[i], root);
const setTextAt = (update, path) =>
  update.patches.find((p) => p.op === "setText" && p.path.join(",") === path.join(","));

// View paths (from counter.zu's tree): count text [2,1,0], note text [4,0],
// "-" button [2,0], "+" button [2,2], reset button [3].
const COUNT = [2, 1, 0];
const NOTE = [4, 0];

// --- checks --------------------------------------------------------------

const a = await session();
const init = decodeInit(a.app.init());
check("init: root is div.counter", init.root.tag === "div" && init.root.attrs.class === "counter");
check("init: count renders 0", nodeAt(init.root, COUNT).text === "0");
check("init: click listener requested", init.events.some((e) => e.name === "click"));
check("init: no cmds/subs for counter", init.cmds.length === 0 && init.subs.length === 0);

const inc = await a.click([2, 2]);
check("+ click: server patches count to 1", setTextAt(inc, COUNT)?.text === "1");

const dec = await a.click([2, 0]);
check("- click: back to 0", setTextAt(dec, COUNT)?.text === "0");

// walk to 10: the conditional note flips exactly when 9 -> 10
let sawNoteAtTen = false;
for (let n = 1; n <= 10; n++) {
  const u = await a.click([2, 2]);
  const note = setTextAt(u, NOTE);
  if (n === 10) sawNoteAtTen = note?.text === "double digits!";
  else if (note && note.text !== "") sawNoteAtTen = false;
}
check("note flips to 'double digits!' exactly at 10", sawNoteAtTen);

// a second connection has its own Program: state is per-socket
const b = await session();
check("conn B: fresh init at 0", nodeAt(decodeInit(b.app.init()).root, COUNT).text === "0");
const bInc = await b.click([2, 2]);
check("conn B: increments to 1 independently", setTextAt(bInc, COUNT)?.text === "1");
const aReset = await a.click([3]);
check("conn A: reset to 0 (B unaffected next)", setTextAt(aReset, COUNT)?.text === "0");
const bInc2 = await b.click([2, 2]);
check("conn B: still at its own 2", setTextAt(bInc2, COUNT)?.text === "2");

// clicking a node with no listener round-trips an empty update, no crash
const noop = await a.click([1]);
check("listener-less path: empty update", noop.patches.length === 0);

// static assets served
for (const f of ["zumar.js", "zumar-wire.js", "zumar-live.js"]) {
  const r = await fetch(`${BASE}/www/${f}`);
  check(`GET /www/${f}`, r.ok && (await r.text()).length > 0);
}

// latency feel: full click round-trip (frame up, dispatch+view+diff on the
// server, patch frame down) over loopback
const N = 200;
const t0 = performance.now();
for (let i = 0; i < N; i++) await a.click([2, 2]);
const rt = (performance.now() - t0) / N;
console.log(`round-trip: ${rt.toFixed(2)} ms/click over ${N} clicks (loopback)`);
check("round-trip under 5ms on loopback", rt < 5);

a.app.close();
b.app.close();
stop();

console.log(`\n${passed}/${passed + failed} checks passed`);
process.exit(failed ? 1 : 0);
