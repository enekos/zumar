# WasmGC backend — spike findings

Two spikes, run in order. Both artifacts live in this directory.

## Spike 1: hand-written WAT (`counter.wat`, `run.mjs`)

A hand-written GC module (state in a `struct` behind a typed global,
renders serialized to linear memory) decodes with the real
`www/zumar-wire.js` under Node. Settled the basics: WasmGC runs where we
need it, and the boundary shape (GC heap for state, linear memory for the
wire buffer) works.

## Spike 2: emitted from the real AST (`zuc-gc`, `run-emitted.mjs`)

`crates/zumar-wasmgc` compiles `counter.zu` — through the real zumar-lang
frontend — straight to a WasmGC binary via the `wasm-encoder` crate. No
Rust toolchain, no wasm-bindgen, no runtime crate: the module is the whole
app. **1,253 bytes**, versus ~57 KB for the same app through the Rust
backend. Passes `wasm-tools validate` and a 14-assertion behavior harness
(bubbled events, negative itoa, nested-if conditional text, no-op on
unhandled paths) through the real wire decoder.

## What the second spike settled

**The runtime.wasm question is retired.** The first spike assumed phase 3
needed the Rust runtime compiled to WasmGC — which rustc can't do. The way
out is better than the workaround: because `.zu` views are statically
known, the compiler emits a **compile-time patch plan**. It knows at
compile time which text nodes are dynamic and where they live; `dispatch`
runs the update against the GC model struct and re-serializes exactly
those texts as SetText patches. No vdom in memory, no diff at runtime, no
runtime port. (This is the Svelte insight applied to the zumar protocol.)

**The no-glue boundary.** wasm-bindgen glue is replaced by four raw
exports:

- `init() -> len` — wire-encoded InitialRender at `mem[0..len]`
- `dispatch(event_idx, path_len) -> len` — `event_idx` indexes the events
  array from the init message; the host writes the path's u32s at
  `path_buf` first
- `mem`, `path_buf`

Handler resolution compiles to a deepest-first prefix match over the
static handler table — same bubbling semantics as the runtime, no vdom
walk. The shim change to support this interface next to the wasm-bindgen
one is small (write path to `path_buf` instead of passing a `Uint32Array`).

## What's still ahead for a real backend

- **Dynamic structure**: `for` list rendering breaks "static tree". The
  plan shape: each `for` region becomes a compile-time region marker;
  dispatch re-serializes the region's children and emits a Replace (or
  truncate+append) on that parent — still no general diff, keyed moves
  can come later.
- **Strings and records on the GC heap**: model fields beyond Int need GC
  arrays (i8 arrays for strings) and nested structs; `witoa` generalizes
  to a small emitted stdlib.
- **Payloads**: `value`/`checked`/`key` arrive via a staging buffer like
  the path does.
- **Effects**: cmds/subs serialization is already in the wire format; the
  emitted module just writes them.

Subset today: Int model fields, payload-less messages, static tree,
literal attributes, dynamic text via `show`/`if`. Everything else errors
with "not yet in the wasmgc backend" and the Rust backend remains the
default.

## Repro

```sh
# spike 1
wasm-tools parse counter.wat -o counter.wasm && node run.mjs
# spike 2
cargo run -p zumar-wasmgc --bin zuc-gc -- \
  ../../examples/lang-counter/counter.zu -o counter-emitted.wasm
node run-emitted.mjs
```
