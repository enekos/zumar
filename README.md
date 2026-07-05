# zumar

*zumar* (eu. "elm tree") — an Elm-like UI stack for WebAssembly, built runtime-first.

The bet: the Elm Architecture is the UI model best suited to Wasm's DOM
boundary, because a TEA program never touches the DOM — it emits patch
descriptions. So the runtime + patch protocol is built and stabilized
**first** (proven with Rust-authored apps), and an Elm-like language
targeting WasmGC comes later as a frontend that speaks the same protocol.

## Quick start

```sh
export ZUMAR_HOME=/path/to/zumar   # this repo
cargo install --path crates/zumar-lang   # installs `zuc`

zuc new myapp
cd myapp
zuc dev            # http://127.0.0.1:8900 — edit myapp.zu, saves hot-reload
```

Compile errors show up Elm-style, in the terminal, while the last good
build keeps serving:

```
myapp.zu:11:30: error: model has no field `cuont`
  11 | update Inc = { count = model.cuont + 1 }
                                    ^
```

## Layout

- `crates/zumar-core` — vdom, diff, patch protocol. No DOM, no wasm deps.
- `crates/zumar-runtime` — the model/update/view loop. Still no wasm deps;
  fully testable natively.
- `crates/zumar-lang` — the language: lexer, parser, typechecker, Rust
  backend, `zuc` CLI. WasmGC backend planned behind the same AST.
- `examples/counter` — TEA counter, hand-written Rust.
- `examples/todo` — keyed lists, controlled input, form submit, checkboxes.
- `examples/effects` — clock, stopwatch (model-driven sub lifecycle),
  chained delays, HTTP fetch.
- `examples/lang-counter` — `counter.zu` compiled by `zuc`; the generated
  crate is committed for inspection.
- `www/zumar.js` + `www/zumar-wire.js` — the entire JS half: create nodes,
  apply patches, delegate events, execute commands, decode the wire format.
  Symlinked into each example's `www/`.

## The protocol

Wasm exposes four calls; JS holds no app state. All four return the same
shape: `{ patches, events, cmds, subs }` (init returns `root` instead of
`patches`).

1. `init()` — full initial tree, event specs (`{name, preventDefault}`),
   init-time commands, initial subscription starts.
2. `dispatch(path, event, payload)` — one DOM event, addressed by
   child-index path, with a standard payload envelope (`{value, checked,
   key}` from the target). Handler lookup, bubbling, update, view, and diff
   all happen vdom-side in Wasm.
3. `resolve(id, payload)` — a one-shot command (delay, httpGet) completed.
4. `notify(id, payload)` — a subscription (interval) fired.

No closures, handler ids, or element references ever cross the boundary.
One boundary crossing per event, one string each way.

Effects mirror the events design: `update` returns serializable command
*specs* the shim executes (callbacks stay wasm-side, keyed by id), and
`subscriptions(&model)` is recomputed per render and lifecycle-diffed —
a sub that disappears from the model's wants is torn down automatically.
Wasm-side code never reads a clock or touches IO; time arrives as messages.

Child lists with `.key()`ed elements diff keyed: reorders become
`moveChild` ops, mid-list edits become `insertChild`/`truncateChildren` —
no subtree rebuilds. Handlers declare what they consume (`Simple`,
`WithValue`, `WithChecked`, `WithKey`), so `.on_input(Msg::Draft)` works
directly with enum constructors.

## Run it

```sh
cargo test                                            # native tests
cd examples/counter && wasm-pack build --target web --out-dir www/pkg
python3 -m http.server 8765 -d www                    # then open :8765
```

## Roadmap

1. ~~M1: patch protocol + runtime + counter demo~~ (done 2026-07-05)
2. ~~M2: keyed child diffing; input/form events with payload envelope~~ (done 2026-07-05)
3. ~~M3: commands/subscriptions (delay, httpGet, every) — the effects side of TEA~~ (done 2026-07-05)
4. ~~M4: binary wire format (3.5–6.7× smaller, 2.7–7.5× faster encode than
   JSON), `zumar_app!` macro, JSON removed from the boundary entirely~~ (done 2026-07-05)
5. M5 — the language, staged:
   - ~~phase 1: zumar-lang v0 frontend (lex/parse/typecheck, total updates)
     + Rust-emitting backend + `zuc` CLI; `counter.zu` runs in the browser~~ (done 2026-07-05)
   - phase 2: language growth — msg payloads, `onInput`/forms, keyed `for`
     over lists (unlocks todo.zu), effects syntax
   - phase 3: WasmGC-direct backend behind the same AST (drops the Rust
     toolchain from the user loop; js-string-builtins for the boundary)

## zumar-lang at a glance

```
app Counter
model { count: Int }
init = { count = 0 }
msg Inc | Dec | Reset
update Inc = { count = model.count + 1 }
update Dec = { count = model.count - 1 }
update Reset = { count = 0 }
view =
  div [class "counter"] [
    button [onClick Dec] [ text "-" ],
    span [] [ text show(model.count) ],
    button [onClick Inc] [ text "+" ]
  ]
```

`zuc check counter.zu` typechecks (every message must have an update
equation — no click can hit a hole); `zuc build counter.zu --out app`
emits a Rust crate speaking the zumar protocol; `zuc dev` builds,
serves, watches, and live-reloads.

## Performance

Measured on `cargo run --release --example diff_bench` / `wire_bench`
(Apple Silicon):

- Diff, 5,000 keyed items: unchanged 207 µs · one edit 258 µs · **full
  reversal 1.2 ms** (keyed matcher is O(n log n): candidate queues +
  Fenwick position tracking, with a reusable-in-place prefix fast path).
- Wire encoding vs JSON: 3.5–6.7× smaller, 2.7–7.5× faster encode; a
  typical per-click update is ~40 bytes. No JSON crosses the Wasm
  boundary in either direction.
- One boundary crossing per event; JS holds no app state.

## Correctness & security

- **Differential fuzz harness** (`crates/zumar-core/tests/diff_apply.rs`):
  a pure-Rust patch applier mirroring `zumar.js` op-for-op; thousands of
  random tree pairs (duplicate keys, mixed keyed/unkeyed, unicode) plus
  every 4-item keyed permutation pair must converge exactly. Runs in CI.
- **XSS hardening** in the shim, same policy as elm/virtual-dom: DOM is
  built only via `createElement`/`createTextNode` (markup in model data is
  inert); `on*` attributes and `srcdoc` never reach the DOM;
  `javascript:`/`data:text/html` URLs are dropped from URL attributes
  (control-char obfuscation included); `<script>` renders inert. Guard
  logic unit-tested in `www/test-guards.mjs`.
- Wire decoder is bounds-checked — truncated or garbage messages throw
  instead of hanging.
- `zuc`'s parser caps nesting depth (clean error, not a stack overflow),
  and its dev server refuses path traversal.
- `#![forbid(unsafe_code)]` on all three crates.
