# zumar

Elm-like UI stack for WebAssembly. *Zumar* is Basque for elm tree.

The starting observation: an Elm program never touches the DOM — update and
view produce descriptions, and something else applies them. That's exactly
the shape Wasm wants, since Wasm can't touch the DOM either. So zumar keeps
the whole application (model, update, view, diffing, event handling) inside
Wasm and crosses the boundary with a compact patch protocol: one call per
event, ~40 bytes for a typical update, no JSON, no closures, no element
references.

It's built runtime-first. The runtime and protocol are ordinary Rust crates
you can write apps against today; zumar-lang, a small Elm-like language, is
a frontend that compiles to the same protocol. Its current backend emits
Rust; a WasmGC backend behind the same AST is the long-term goal.

## Quick start

```sh
export ZUMAR_HOME=/path/to/this/repo
cargo install --path crates/zumar-lang    # installs zuc

zuc new myapp
cd myapp
zuc dev        # http://127.0.0.1:8900, rebuilds on save
```

Compile errors print where they happened, and the last good build keeps
serving while you fix them:

```
myapp.zu:11:30: error: model has no field `cuont`
  11 | update Inc = { count = model.cuont + 1 }
                                    ^
```

## The language

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

Updates are total: declaring a message and not handling it is a compile
error. v0 is deliberately small — one model record (Int/String/Bool),
messages without payloads, expressions with arithmetic, `++`, comparisons,
`if`, `show`. Message payloads and list rendering are next; the todo
example can't be written in zumar-lang yet, which is the point of the next
milestone.

`zuc check` typechecks, `zuc build` emits a Rust crate, `zuc dev` builds,
serves, watches and live-reloads.

## The protocol

Wasm exposes four calls, all returning the same wire-encoded shape
(`patches`, `events`, `cmds`, `subs`):

- `init()` — the initial tree, plus event names to delegate
- `dispatch(path, event, value?, checked?, key?)` — a DOM event
- `resolve(id, ok?, status?, body?)` — a command (timer, HTTP) finished
- `notify(id, now?)` — a subscription fired

Nodes are addressed by child-index paths. Events are resolved inside the
vdom: the JS side installs one delegated listener per event *name* and
reports `(path, name)`; handler lookup and bubbling happen in Wasm.
Effects follow the same split — serializable specs cross the boundary,
message callbacks stay inside. `subscriptions(&model)` is recomputed per
render and diffed, so an interval the model stops asking for is torn down
without bookkeeping. The JS half (`www/zumar.js` + `www/zumar-wire.js`) is
~300 lines, holds no app state, and is the part a language backend keeps
verbatim.

## Crates

- `crates/zumar-core` — vdom, diff, patch types. No wasm, no DOM.
- `crates/zumar-runtime` — the model/update/view loop, effects, wire encoding.
- `crates/zumar-lang` — lexer, parser, typechecker, Rust backend, the `zuc` CLI.
- `examples/` — counter, todo (keyed lists, forms), effects (timers, HTTP),
  lang-counter (compiled from `counter.zu`; generated crate committed for
  inspection).

## Numbers

From `diff_bench` and `wire_bench` on an M-series laptop:

- 5,000 keyed items: unchanged 207 µs, one edit 258 µs, full reversal
  1.2 ms. The keyed matcher runs in O(n log n).
- Wire format vs serde_json: 3.5–6.7× smaller, 2.7–7.5× faster to encode.
  A counter click is about 40 bytes round trip.

## Safety

The diff is checked by a differential harness (`zumar-core/tests/diff_apply.rs`):
a patch applier that mirrors zumar.js op-for-op, run against thousands of
random tree pairs and every permutation of small keyed lists — diff + apply
must reproduce the target tree exactly.

The shim follows elm/virtual-dom's DOM policy: no innerHTML anywhere, `on*`
attributes and `srcdoc` are dropped, `javascript:` URLs are stripped
(control-char obfuscation included), `<script>` renders inert. The wire
decoder rejects truncated input, the parser caps nesting depth, and all
three crates forbid unsafe code.

## Running the examples

```sh
cargo test
cd examples/counter && wasm-pack build --target web --out-dir www/pkg
python3 -m http.server 8765 -d www
```

## Roadmap

1. ~~patch protocol + runtime~~
2. ~~keyed diffing, input/form events~~
3. ~~commands and subscriptions~~
4. ~~binary wire format~~
5. zumar-lang — v0 done; next: message payloads, `onInput`, list rendering
   (unlocks `todo.zu`), then a WasmGC backend behind the same AST.

MIT.
