// zumar-live.js — WebSocket transport adapter for live mode: the program
// runs *in the server* (one Program per connection) and this file presents
// the same app-object interface as zumar-gc.js, except init/dispatch/
// resolve/notify are WS frames instead of wasm calls. Dispatch returns
// nothing; the server's Update arrives as a pushed binary frame and reaches
// zumar.js through the onUpdate hook. Commands and subscriptions still
// execute in the browser (setTimeout/fetch/setInterval) — their resolve/
// notify completions travel back over the socket.
//
// Outbound (client→server) frame format, LEB128 varints + u8 tags; the
// server mirror lives in the sutegi-zumar bridge's frame.rs — keep in
// lockstep:
//   frame    = ver:u8=1 kind:u8 body
//   dispatch = kind 0  n:varint path*n:varint  name:str  flags:u8
//              [value:str] [key:str]
//              flags: bit0 value present · bit1 checked present
//                     bit2 checked value · bit3 key present
//   resolve  = kind 1  id:varint ok:u8 status:varint body:str
//   notify   = kind 2  id:varint now:varint (ms since epoch)
//   str      = len:varint utf8
// Inbound (server→client) frames are standard wire messages: the first is
// an InitialRender, every later one an Update (decoded by zumar-wire.js).

import { mount } from "./zumar.js";

class Writer {
  constructor() {
    this.a = [];
  }
  u8(n) {
    this.a.push(n & 0xff);
    return this;
  }
  vu(n) {
    n = Math.floor(n);
    do {
      const b = n % 128;
      n = Math.floor(n / 128);
      this.a.push(n ? b | 0x80 : b);
    } while (n);
    return this;
  }
  str(s) {
    const bytes = new TextEncoder().encode(s);
    this.vu(bytes.length);
    for (const b of bytes) this.a.push(b);
    return this;
  }
  bytes() {
    return new Uint8Array(this.a);
  }
}

// Opens the socket and resolves to a mountable app object once the server's
// initial render has arrived (so app.init() can answer synchronously).
// `app.closed` is a promise that settles when the socket later drops —
// what mountLive's reconnect loop awaits.
export function connect(url) {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(url);
    ws.binaryType = "arraybuffer";
    let initBytes = null;
    let push = null;
    let onClosed = null;
    const closed = new Promise((r) => (onClosed = r));
    const send = (w) => ws.send(w.bytes());

    const app = {
      init: () => initBytes,
      dispatch(path, name, value, checked, key) {
        const w = new Writer().u8(1).u8(0).vu(path.length);
        for (const p of path) w.vu(p);
        w.str(name);
        let flags = 0;
        if (typeof value === "string") flags |= 1;
        if (typeof checked === "boolean") flags |= 2 | (checked ? 4 : 0);
        if (typeof key === "string") flags |= 8;
        w.u8(flags);
        if (flags & 1) w.str(value);
        if (flags & 8) w.str(key);
        send(w);
      },
      resolve(id, ok, status, body) {
        send(
          new Writer()
            .u8(1)
            .u8(1)
            .vu(id)
            .u8(ok ? 1 : 0)
            .vu(status | 0)
            .str(typeof body === "string" ? body : "")
        );
      },
      notify(id, now) {
        send(new Writer().u8(1).u8(2).vu(id).vu(typeof now === "number" ? now : 0));
      },
      onUpdate(fn) {
        push = fn;
      },
      close() {
        ws.close(1000, "");
      },
      closed,
    };

    ws.onmessage = (e) => {
      const bytes = new Uint8Array(e.data);
      if (initBytes === null) {
        initBytes = bytes;
        resolve(app);
      } else if (push) {
        push(bytes);
      }
    };
    ws.onerror = () => reject(new Error(`zumar-live: connection failed: ${url}`));
    ws.onclose = (e) => {
      if (initBytes === null) {
        // Surface the close code so mountLive can stop retrying a policy
        // rejection (1008 — the server's Live::guard refused this session).
        const err = new Error("zumar-live: closed before init");
        err.code = e.code;
        reject(err);
      }
      onClosed();
    };
  });
}

// Mount with a persistent session and auto-reconnect. The model lives in
// the server (and, with a journal, survives the socket): each reconnect
// carries the same `?session=` id, the server replays the journal, and the
// page remounts the fresh full render on a clean root. Never returns.
//
// opts: sessionKey (localStorage key, default "zumar-live-session"),
//       session: false to disable the session id entirely.
export async function mountLive(url, root, opts = {}) {
  const full = opts.session === false ? url : withSession(url, opts.sessionKey);
  let attempt = 0;
  for (;;) {
    try {
      const app = await connect(full);
      attempt = 0;
      const fresh = root.cloneNode(false); // drop old listeners + children
      root.replaceWith(fresh);
      root = fresh;
      mount(app, root, opts);
      await app.closed;
    } catch (e) {
      // A policy close (1008) means Live::guard rejected us — retrying can't
      // help until the user logs in, so stop rather than spin.
      if (e && e.code === 1008) {
        if (opts.onUnauthorized) opts.onUnauthorized();
        return;
      }
      // otherwise fall through to backoff
    }
    attempt += 1;
    await new Promise((r) => setTimeout(r, Math.min(250 * 2 ** attempt, 5000)));
  }
}

function withSession(url, key = "zumar-live-session") {
  let id = null;
  try {
    id = localStorage.getItem(key);
    if (!id) {
      id = [...crypto.getRandomValues(new Uint8Array(16))]
        .map((b) => b.toString(16).padStart(2, "0"))
        .join("");
      localStorage.setItem(key, id);
    }
  } catch {
    return url; // no storage (file://, privacy mode) → ephemeral session
  }
  return url + (url.includes("?") ? "&" : "?") + "session=" + id;
}
