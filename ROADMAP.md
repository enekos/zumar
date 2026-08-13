# zumar — roadmap

*An Elm-like UI stack for WebAssembly, built runtime-first: a vdom patch protocol and TEA
runtime in Rust, plus a small language (`zuc`) that compiles to it.*

**Updated** 2026-08-13 · ~18k LOC, 244 tests · **last commit 2026-07-07 — 37 days ago**

---

## Where this actually is

- **The hard parts work.** A TEA runtime over a binary patch protocol; a real language with a
  parser, type checker, and code generator; two backends behind one AST (Rust/wasm-bindgen
  and WasmGC) with staged emission via `wasm-encoder` rather than shelling out; effects split
  into spec and callback; path-addressed keyed diffing; decl fragments so a server-generated
  `schema.zu` type-checks every page with per-file caret attribution.
- **The full-stack integration is complete and it is the impressive part.** P0 through P5
  with sutegi: scaffolding and a 4–12 ms `.zu` rebuild dev loop, cross-tier type checking
  (a renamed database column becomes a frontend caret error — proven live), a live bridge
  that runs one `Program` per WebSocket connection with server-side effects and
  reconnect-by-replay through an event journal (reload the tab, state comes back —
  Chrome-verified), `topic()`/`publish()` driving a multi-client chat written in `.zu`, and
  `httpPost` with guarded live mounts giving CSRF-free forms by construction. 0.10 ms
  loopback round-trip per click.
- **It has been dormant for over a month.** In that time sutegi — its integration partner —
  shipped three releases, and bildu shipped twenty. zumar is the only project here that has
  gone a month without a commit while its dependents kept moving.
- **Its only consumers are a dormant art project and a bridge.** horma exercises it; horma is
  also idle. Nothing in daily use is written in `.zu`.
- **Not published.** zumar-core and zumar-runtime are not on crates.io. The one item on the
  old list marked *"irreversible — needs go-ahead"* is the only one that would make this
  usable by anyone.

## The one thing that decides this project

**Is zumar a language project, or was it a spike that already proved its point?**

The spike succeeded. Option C — runtime first, language later — was chosen so demos would
exist from week one, and they do. The thesis ("a TEA program emits patch descriptions instead
of touching the DOM, which is exactly what suits Wasm's DOM boundary") is demonstrated,
end to end, including the genuinely novel result: **the LiveView analog is the language
itself**, and a `.zu` file runs identically in the browser and on the server.

That is a finished, publishable piece of work.

Being a *language* is a different commitment entirely: whitespace decisions, error messages,
an editor plugin, a package story, documentation, and years of patience. One month of silence
is data about appetite for that.

**Two honest paths. Pick one; do not drift.**

- **Ship the finding.** Write it up — "WasmGC plus TEA plus a binary patch protocol, and the
  server-rendered mode falls out for free" is a genuinely interesting post. Publish the
  crates, archive the repository as a completed experiment. Cost: about a week. This is the
  recommended path.
- **Commit to the language.** Then M1 below is mandatory and the bar is a real application
  written in `.zu` that you use every day.

---

## M1 — Close the dogfood loop, or close the project ← next

The highest-signal work available, and it doubles as the decision procedure. horma's
hand-written Rust `Program` is the exit test that was already defined: rewriting it in `.zu`
either works, or produces the list of what the language is actually missing.

- [ ] Shared server state readable from `view` (horma's wall snapshot).
- [ ] String and structured parsing (topic payloads, fetch bodies).
- [ ] Mouse down/move/up events in the language. Wheel direction already rides the key slot.
- [ ] Tuple and multi-argument msg payloads.
- [ ] **Exit test: rewrite horma's Program in `.zu` and run it.**

**Done when:** horma runs on `.zu` — or the attempt is abandoned with a written list of why,
which is the trigger to take the "ship the finding" path.

## M2 — Publish, so it can be evaluated

Irreversible, and that is the point: an unpublished language is a private art project.

- [ ] Publish `zumar-core` and `zumar-runtime` to crates.io.
- [ ] A README that leads with the demo and the 0.10 ms number, not the architecture.
- [ ] The write-up. WasmGC shipped in every browser (Safari 18.2 was the last), which removed
      the historical blocker for GC'd languages on Wasm. Very few people have built on that
      yet, and fewer have noticed that TEA makes the server-rendered mode free.

**Done when:** a stranger can `cargo add zumar-runtime`, and there is a URL explaining why
they would.

## M3 — GC backend leftovers

Real gaps, worth closing only if M1 chose the language path.

- [ ] Enum payload variants.
- [ ] Nested `for`, and `for` beside siblings.
- [ ] `sum` and `nth`.
- [ ] `httpPost` with computed bodies (currently a clean error on the GC backend).
- [ ] Helper tree-shaking.

## M4 — P6 single-binary deploy

Driven from sutegi's side, not this repository. horma's `include_str!` embed is the proven
pattern; productizing it is a sutegi scaffold option.

---

## Open design questions

- **GC backend `for` regions.** Replace-the-parent is simple and loses keyed moves. Where is
  the line at which a compiled keyed algorithm earns its bytes? Unanswerable without a real
  application — another argument for M1.
- **Whitespace sensitivity.** v0 is Elm-adjacent but bracket-delimited, with no layout
  parsing. Revisit only if real programs get noisy. They cannot get noisy until they exist.

## Not doing

- **A `sutegi-live`.** This bridge *is* the LiveView analog. That was decided and it was
  right.
- **Competing with Elm.** Elm has a decade of error-message craft and a community. zumar's
  claim is the Wasm boundary and the one-language-two-runtimes property, not general
  ergonomics.
- **A JavaScript interop story.** The binary wire format is the boundary. Widening it
  dissolves the thesis.

## Risks worth naming

- **Dormancy is the actual risk, and it is already realised.** 37 days. The sutegi bridge is
  workspace-excluded and needs a sibling checkout to build, so it is exactly the kind of
  integration that rots silently while both sides move.
- **Bus factor 1 on a compiler.** Parser, type checker, two code generators, and a binary wire
  protocol. Every month of silence makes the next contributor — including future you — more
  expensive.
- **The window is open now.** WasmGC availability is recent and the space is thin. That is a
  reason to publish this year, not eventually.
