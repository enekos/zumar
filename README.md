# zumar

*zumar* (eu. "elm tree") — an Elm-like UI stack for WebAssembly, built runtime-first.

The bet: the Elm Architecture is the UI model best suited to Wasm's DOM
boundary, because a TEA program never touches the DOM — it emits patch
descriptions. So the runtime + patch protocol is built and stabilized
**first** (proven with Rust-authored apps), and an Elm-like language
targeting WasmGC comes later as a frontend that speaks the same protocol.

## Layout

- `crates/zumar-core` — vdom, diff, patch protocol. No DOM, no wasm deps.
- `crates/zumar-runtime` — the model/update/view loop. Still no wasm deps;
  fully testable natively.
- `examples/counter` — TEA counter compiled to Wasm via wasm-bindgen.
- `examples/todo` — keyed lists, controlled input, form submit, checkboxes.
- `examples/effects` — clock, stopwatch (model-driven sub lifecycle),
  chained delays, HTTP fetch.
- `www/zumar.js` — the entire JS half (~190 lines): create nodes, apply
  patches, delegate events, execute commands, manage interval handles.
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
4. M4: binary patch encoding, benchmark vs JSON
5. M5: language frontend targeting WasmGC, emitting the same protocol
