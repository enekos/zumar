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
cargo install --path crates/zumar-cli     # installs zuc

zuc new myapp
cd myapp
zuc dev --backend gc    # http://127.0.0.1:8900 — millisecond rebuilds,
                        # no cargo or wasm-pack in the loop
```

The GC backend compiles `.zu` straight to a self-contained WasmGC module,
so a save-to-reload cycle is ~1ms. The default Rust backend (`zuc dev`)
goes through cargo + wasm-pack and needs `ZUMAR_HOME=/path/to/this/repo`;
it remains the reference implementation and covers the few things the GC
backend doesn't yet (enum payload variants, `Maybe`).

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
error. The language has scalars (Int/String/Bool), `record` types, `List`,
message payloads, and a small functional layer — list literals, record
literals and updates (`{ x | field = e }`), and comprehensions
(`for x in xs where c yield e`). Events cover `onClick`/`onChange`/`onSubmit`
(with a message value) and `onInput`/`onCheck` (constructor takes the event
payload); `for x in xs { <element> }` renders keyed lists.

The full todo app — the phase-2 target — is now expressible:

```
record Item { id: Int, text: String, done: Bool }
model { draft: String, items: List Item, seq: Int }

update Add = {
  items = model.items ++ [{ id = model.seq, text = model.draft, done = false }],
  seq = model.seq + 1, draft = ""
}
update Toggle id = {
  items = for t in model.items yield (if t.id == id then { t | done = not t.done } else t)
}
update Delete id = { items = for t in model.items where t.id != id yield t }
```

See `examples/lang-todo/todo.zu` for the whole thing.

`zuc check` typechecks, `zuc build` emits a Rust crate, `zuc dev` builds,
serves, watches and live-reloads.

List helpers: `length`, `sum` (over a `List Int`), `reverse`, `nth(list, i,
default)`, `head(list) -> Maybe T`, and
`fold(list, init, acc x -> expr)` — the lambda is syntactic and always
applied, so the language stays first-order and the GC backend inlines it
to a loop. `toInt(s)` parses a `String` to `Int` (0 on failure) — see
`examples/lang-expenses`.

`Maybe T` is a built-in optional (Rust `Option` under the hood), built with
`some(x)` / `none` and taken apart with `case`, which must handle both arms:

```
text (case head(model.jobs) of
        none -> "nothing queued"
        | some j -> "next up: " ++ j.label)
```

So reading the front of a possibly-empty list can't skip the empty case —
the same totality rule as messages, extended to optionals. See
`examples/lang-queue`.

Effects are part of the language: an update (or `init`) can request
commands with `then`, and a `sub` declaration derives subscriptions from
the model — the runtime starts and stops them as the model changes:

```
init = { ... } then httpGet("./quote.txt", Got)

update Ping = { pinged = "ping..." } then delay(1500, Pong)

sub = if model.running then [ every(1000, Tick) ] else []
```

`httpGet(url, Ctor)` delivers the body to a String-payload message (or
`"error <status>"` on failure); `every(ms, Msg)` fires a payload-less
message, or passes the clock when the message takes an Int. Arithmetic
includes `/` and `%`, with division by zero yielding 0, Elm's rule. See
`examples/lang-clock`.

Sum types are declared with `enum`; `case` works over them and must handle
every variant — dropping one is a compile error, the same totality rule as
messages and `Maybe`:

```
enum Status = Todo | Doing | Done

update Advance id = {
  tasks = for t in model.tasks yield
    (if t.id == id
     then { t | status = case t.status of Todo -> Doing | Doing -> Done | Done -> Todo }
     else t)
}
```

Variants may carry one payload (`enum Filter = All | ByOwner String`),
bound in the arm (`ByOwner o -> ...`); constructors share one namespace.
Payload-less enums compare with `==` and run on both backends (i32 tags on
GC) — see `examples/lang-kanban`.

### Not there yet

Enum payload variants and nested `for` don't reach the GC backend yet
(clean errors point back to the Rust backend), and there are no
general-purpose lambdas — `fold`'s arrow is the only one.

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
- `crates/zumar-lang` — lexer, parser, typechecker, Rust backend.
- `crates/zumar-cli` — the `zuc` CLI: check/build/scaffold + the dev server,
  with `--backend gc` for rustc-free millisecond rebuilds.
- `crates/zumar-wasmgc` — the WasmGC backend: `zuc-gc` compiles `.zu`
  straight to a self-contained GC binary via `wasm-encoder` — no Rust
  toolchain, no wasm-bindgen, no runtime. Records are GC structs, lists are
  GC arrays, strings are `array i8`. Instead of a runtime diff it emits a
  compile-time patch plan; a `for` becomes a region that re-serializes into
  one Replace patch, and handlers inside it resolve their message argument
  (`Toggle(t.id)`) from the clicked item's path index at dispatch time. The
  full todo app is 3 KB (vs ~57 KB through the Rust backend); counter is
  1.7 KB. `www/zumar-gc.js` adapts the raw exports to the standard shim, so
  GC modules run in normal browser pages. Effects work without a runtime
  too: command ids encode their compile-time callsite, subscriptions keep a
  wanted-vs-active bitmask emitting start/stop deltas — clock.zu with its
  interval, fetches, and delay chain is 2.7 KB. Not yet there: `Maybe`,
  Bool payloads, nested `for`.
- `examples/` — counter, todo (keyed lists, forms), effects (timers, HTTP),
  and lang-counter / lang-todo / lang-expenses / lang-queue / lang-clock /
  lang-kanban (compiled from `.zu`).
- `spikes/wasmgc` — findings + harnesses for the GC backend.

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
5. ~~zumar-lang v0~~ + ~~phase 2: records, lists, payloads, comprehensions
   (`todo.zu`)~~ + ~~phase 2.1: `sum`, `nth`, `toInt` (`expenses.zu`)~~ +
   ~~phase 2.2: `Maybe` + `case` (`queue.zu`)~~
6. ~~WasmGC backend spike: `zuc-gc` emits the counter subset as a 1.3 KB
   self-contained GC module (compile-time patch plans instead of a runtime
   diff; findings in `spikes/wasmgc`)~~
7. ~~GC backend: strings, records, `for` regions, payloads — counter,
   hello, and todo all run on it~~
8. ~~effects in the language: `then` commands, `sub` subscriptions, `/`
   and `%` (`clock.zu`)~~ + ~~effects on the GC backend~~
9. ~~user-defined sum types + general pattern matching (`kanban.zu`, both
   backends)~~
10. ~~`zuc dev --backend gc` — millisecond rebuilds~~
11. ~~`fold`; GC: `Maybe` (null-repr), Bool payloads, `toInt` — every demo
    app now runs on both backends~~
12. next: sutegi integration (full-stack story); GC gaps (enum payloads,
    nested `for`, helper tree-shake).

MIT.
