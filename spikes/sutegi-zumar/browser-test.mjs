// Real-browser check for the P0 spike: headless Chrome driven over the CDP
// WebSocket (no test deps — Node's built-in WebSocket). Loads the live page,
// clicks the real buttons, and asserts the DOM the server's patches produce.
// This is the one path run.mjs can't cover: zumar.js mount() + zumar-live.js
// wired together against a real DOM.
//
//   node browser-test.mjs

import { spawn, spawnSync } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const PORT = process.env.PORT || "8792";
const CDP = "9223";
const CHROME = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";

let passed = 0;
let failed = 0;
const check = (name, cond) => {
  if (cond) passed++;
  else failed++;
  console.log(`${cond ? "ok" : "FAIL"} - ${name}`);
};

const build = spawnSync("cargo", ["build", "-q"], { cwd: here, stdio: "inherit" });
if (build.status !== 0) process.exit(1);

const server = spawn(join(here, "target/debug/sutegi-zumar"), [], {
  env: { ...process.env, HOST: "127.0.0.1", PORT },
  stdio: "ignore",
});
const chrome = spawn(
  CHROME,
  [
    "--headless=new",
    `--remote-debugging-port=${CDP}`,
    `--user-data-dir=/tmp/sutegi-zumar-chrome`,
    "--no-first-run",
    "about:blank",
  ],
  { stdio: "ignore" }
);
const cleanup = () => {
  server.kill("SIGTERM");
  chrome.kill("SIGTERM");
};
process.on("exit", cleanup);

const until = async (f, what) => {
  for (let i = 0; i < 100; i++) {
    try {
      const v = await f();
      if (v) return v;
    } catch {}
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error(`timed out waiting for ${what}`);
};

await until(() => fetch(`http://127.0.0.1:${PORT}/`).then((r) => r.ok), "server");
const target = await until(
  () =>
    fetch(`http://127.0.0.1:${CDP}/json/new?http://127.0.0.1:${PORT}/`, { method: "PUT" }).then(
      (r) => r.json()
    ),
  "chrome CDP"
);

const ws = new WebSocket(target.webSocketDebuggerUrl);
await new Promise((r, j) => {
  ws.onopen = r;
  ws.onerror = j;
});

let msgId = 0;
const pending = new Map();
ws.onmessage = (e) => {
  const m = JSON.parse(e.data);
  if (pending.has(m.id)) {
    pending.get(m.id)(m);
    pending.delete(m.id);
  }
};
const evaluate = (expression) =>
  new Promise((resolve) => {
    const id = ++msgId;
    pending.set(id, (m) => resolve(m.result?.result?.value));
    ws.send(JSON.stringify({ id, method: "Runtime.evaluate", params: { expression } }));
  });

const count = () => evaluate(`document.querySelector(".count")?.textContent`);
const click = (sel) =>
  evaluate(
    `(() => { const b = document.querySelector(${JSON.stringify(sel)}); if (b) b.click(); return !!b; })()`
  );

await until(async () => (await count()) === "0", "live mount (count = 0)");
check("page mounts over WS: count is 0", true);

await click(".row button:last-child"); // the "+" button
await until(async () => (await count()) === "1", "count = 1 after + click");
check("real click on + patches DOM to 1", true);

await click(".row button:last-child");
await until(async () => (await count()) === "2", "count = 2");
check("second click reaches 2", true);

await click(".row button:first-child"); // the "-" button
await until(async () => (await count()) === "1", "count = 1 after -");
check("- click patches DOM back to 1", true);

await click("button.reset");
await until(async () => (await count()) === "0", "count = 0 after reset");
check("reset click patches DOM to 0", true);

ws.close();
cleanup();
console.log(`\n${passed}/${passed + failed} browser checks passed`);
process.exit(failed ? 1 : 0);
