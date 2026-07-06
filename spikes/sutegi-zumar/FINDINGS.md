# P0 spike findings — counter.zu live from a sutegi server

2026-07-06. Gate for the sutegi+zumar full-stack plan
(thinking-os: decisions/zumar-sutegi-fullstack-2026-07-06.md). **Passed.**

## What runs

One `zumar_runtime::Program` per `sutegi-ws` connection. The browser runs
the stock zumar.js shim + a new `www/zumar-live.js` adapter (same
app-object interface as zumar-gc.js); DOM events go up as binary frames,
wire-format patches stream back down. Model state never leaves the server.

- `node run.mjs` — 16 checks through the real adapter + wire decoder:
  init render, click round-trips, the conditional note flipping exactly at
  10, per-connection state isolation (two sockets, independent counts),
  malformed-path no-ops, static serving, latency.
- `node browser-test.mjs` — 5 checks in headless Chrome over CDP (no test
  deps): real button clicks patch a real DOM via the full stack.
- `cargo test` — 4 frame-decoder tests (malformed frames error, never panic).

## Numbers

- **Round-trip 0.10 ms/click** over loopback (frame up → dispatch + view +
  diff on the server → patch frame down), 200-click average, debug build.
  Latency feel is a non-issue at LAN scale; the budget is the network.
- Server holds `Program` + current vdom per connection; memory at 10k+
  conns still unmeasured (plan open question 2).

## Answers to the plan's open questions

1. **Frame layout: one wire message per WS frame.** No length-prefix
   multiplexing needed — sutegi-ws already preserves message boundaries.
   First frame down is an `InitialRender`, every later one an `Update`;
   client→server frames are `ver kind body` (see `src/frame.rs` ↔
   `www/zumar-live.js`, kept in lockstep). Multiplexing only becomes a
   question for hybrid pages with several programs per socket — defer.
2. **`zuc --target live` is unnecessary (favored answer confirmed).** The
   bridge consumes the existing Rust-backend output verbatim: counter.rs
   here is the zuc-generated file with the `zumar_app!` wasm-bindgen
   wrapper swapped for a 3-line `program()` constructor. zuc could emit
   that constructor alongside the macro call for free (`--emit program-fn`
   or just always).
3. **The adapter seam cost one small zumar.js change**, not zero: mount()
   assumed every app call returns update bytes synchronously. It now
   tolerates async transports — a call may return nothing, and pushed
   updates arrive via an optional `app.onUpdate(fn)` hook. Sync transports
   (wasm, gc) are unaffected. This is the entire framework-side diff.
4. **Effects work in live mode for free, executed client-side.** Update
   frames carry cmd/sub specs; zumar.js still runs setTimeout/fetch/
   setInterval and the resolve/notify completions travel back over the
   socket into the server-side Program. So clock.zu would already run live
   today — P3's server-side effects are an upgrade (server-local httpGet,
   timer wheel, fewer round-trips), not a prerequisite.

## Gaps / next (feed into P1–P3)

- Reconnect = fresh Program (state lost). The event-sourcing replay story
  is P3's novel piece.
- No session/auth params at mount; `on_open`'s `&Request` is the hook.
- The per-connection map is a hand-rolled `static`; the real bridge crate
  should own connection lifecycle + supervision.
- serde still compiled into zumar-runtime server-side (plan: feature-gate).
- Inbound frames are trusted after decode; path depth capped at 64,
  strings bounded by ws max_message (1 MiB default).
