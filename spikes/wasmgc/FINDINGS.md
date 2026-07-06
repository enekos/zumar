# WasmGC spike — findings

Ran a hand-written WasmGC module (`counter.wat`) whose state is a GC
`struct` behind a typed global, serializing renders into linear memory.
Decoded its output with the real `www/zumar-wire.js`. It passes.

## What this settles

**WasmGC runs where we need it.** Node 25 executes `struct.new/get/set` and
typed globals with no flags; browser support was already there (Chrome/FF
since 2023, Safari 18.2). No blocker.

**The boundary doesn't change.** The module keeps state on the GC heap but
still writes the wire buffer to linear memory and returns `(offset, len)` —
the same shape wasm-bindgen produces today. So the wire format, the JS shim,
and the decoder are all reused verbatim; phase 3 touches only the backend.
That's the runtime-first bet paying off a second time.

## Emission strategy (the question the spike was for)

Three options considered:

1. **Emit WAT text, shell out to `wasm-tools`.** Readable, trivial to debug,
   but puts an external binary in the user's build loop.
2. **Hand-roll a binary encoder** (LEB128, sections, GC type indices).
   Self-contained but a lot of fiddly code to get right.
3. **Use the `wasm-encoder` crate** (from the wasm-tools project) to emit
   binary directly from `zuc`. No external process, no hand-rolled LEB128,
   and it's the same well-tested encoder `wasm-tools` uses.

**Decision: option 3.** `zuc` links `wasm-encoder` and emits WasmGC binary
directly; keep a `--emit wat` debug flag that goes through option 1 for
eyeballing output.

## The real phase-3 design problem

The current Rust backend gets the entire runtime — diff, effects, wire
encoding, event resolution — for free by linking `zumar-core`/`zumar-runtime`.
A WasmGC backend can't link Rust crates. Two ways out:

- **Re-emit the runtime per app** — `zuc` generates diff/wire/loop code into
  every module. Large modules, and the tested runtime gets duplicated by a
  second implementation. Rejected.
- **Precompiled `runtime.wasm`** — compile `zumar-core`/`zumar-runtime` to
  WasmGC *once*, ship it, and have `zuc` emit only the app module
  (model/update/view) which imports the runtime's exports. Small per-app
  output, one shared tested runtime. This is the path.

So phase 3 is really two pieces: (a) a `runtime.wasm` build of the existing
crates with a stable import interface, and (b) a `zuc` backend that emits a
small app module against it. (a) is the load-bearing unknown — worth its own
spike before committing.

## Cost

Getting a counter through option 3 against a precompiled runtime is a
week-ish of focused work. Full parity with the Rust backend (records, lists,
comprehensions lowered to GC arrays) is more. The Rust backend stays the
default until the WasmGC path reaches parity.

## Repro

```sh
wasm-tools parse counter.wat -o counter.wasm
node run.mjs
```
