#![forbid(unsafe_code)]
//! zumar-lang — an Elm-like language compiling to zumar TEA apps.
//!
//! Pipeline: [`parse`] → [`check`] → [`generate`]. The frontend is
//! backend-agnostic; today's backend emits a Rust crate built with the
//! existing wasm toolchain, and a WasmGC-direct backend is planned behind
//! the same AST (the reason zumar was built runtime-first).
//!
//! The language has records, `List`, message payloads, comprehensions, and
//! an element tree with events and list rendering. It is total by
//! construction: every message must have an update equation.

pub mod ast;
pub mod check;
pub mod gen;
pub mod lex;
pub mod parse;

pub use ast::{App, ZuError};
pub use gen::Generated;

/// Parse + typecheck (resolving named types to records/enums).
pub fn compile(source: &str) -> Result<App, ZuError> {
    let mut app = parse::parse(source)?;
    check::check(&mut app)?;
    Ok(app)
}

/// Shared type declarations parsed from a fragment (see
/// [`parse::parse_decls`]) — the cross-tier seam: a server generates these
/// from its DB schema, and every page compiles against them.
pub struct Decls {
    pub records: Vec<ast::RecordDef>,
    pub enums: Vec<ast::EnumDef>,
}

/// Parse a `record`/`enum`-only fragment.
pub fn parse_decls(source: &str) -> Result<Decls, ZuError> {
    let (records, enums) = parse::parse_decls(source)?;
    Ok(Decls { records, enums })
}

/// Compile a program with extra shared declarations merged in before the
/// typecheck — so user code referencing a generated record fails to compile
/// the moment the schema changes underneath it. The program's own
/// declarations win no special treatment: a name declared twice is the same
/// duplicate error it always was.
pub fn compile_with(source: &str, decls: Decls) -> Result<App, ZuError> {
    let mut app = parse::parse(source)?;
    app.records.extend(decls.records);
    app.enums.extend(decls.enums);
    check::check(&mut app)?;
    Ok(app)
}

#[cfg(test)]
mod tests {
    use super::*;

    const COUNTER: &str = r#"
# the canonical zumar-lang program
app Counter

model { count: Int }

init = { count = 0 }

msg Inc | Dec | Reset

update Inc = { count = model.count + 1 }
update Dec = { count = model.count - 1 }
update Reset = { count = 0 }

view =
  div [class "counter"] [
    h1 [] [ text "zumar-lang" ],
    div [class "row"] [
      button [onClick Dec] [ text "-" ],
      span [class "count"] [ text show(model.count) ],
      button [onClick Inc] [ text "+" ]
    ],
    button [class "reset", onClick Reset] [ text "reset" ],
    p [class "note"] [ text (if model.count > 9 then "double digits!" else "") ]
  ]
"#;

    const TODO: &str = r#"
app Todo
record Item { id: Int, text: String, done: Bool }
model { draft: String, items: List Item, seq: Int }
init = { draft = "", items = [], seq = 1 }
msg Draft String | Add | Toggle Int | Delete Int | Reverse
update Draft s = { draft = s }
update Add = {
  items = model.items ++ [{ id = model.seq, text = model.draft, done = false }],
  seq = model.seq + 1,
  draft = ""
}
update Toggle id = {
  items = for t in model.items yield (if t.id == id then { t | done = not t.done } else t)
}
update Delete id = { items = for t in model.items where t.id != id yield t }
update Reverse = { items = reverse(model.items) }
view =
  ul [class "items"] [
    for t in model.items {
      li [key show(t.id)] [
        span [onClick Toggle(t.id)] [ text t.text ],
        button [onClick Delete(t.id)] [ text "x" ]
      ]
    }
  ]
"#;

    fn gen_rs(src: &str) -> String {
        let app = compile(src).unwrap();
        gen::generate(&app, "zumar-core = {}").lib_rs
    }

    #[test]
    fn both_targets_expose_program_and_gate_wasm() {
        let app = compile(COUNTER).unwrap();
        // Same lib.rs for both: a public `program()` constructor, wasm-bindgen
        // behind a cfg. The client build turns the feature on; the server
        // (sutegi-zumar) leaves it off and mounts `program()` directly.
        let lib = gen::generate_with(&app, "zumar-core = {}", gen::Target::Live).lib_rs;
        assert!(lib.contains("pub fn program() -> zumar_runtime::Program<Model, Msg>"));
        assert!(lib.contains("#[cfg(feature = \"wasm\")]\nuse wasm_bindgen"));
        assert!(lib.contains("#[cfg(feature = \"wasm\")]\nzumar_runtime::zumar_app!"));

        // The manifests are what differ: live keeps wasm-bindgen optional and
        // off, and omits [workspace] so it embeds as a path dep.
        let live = gen::generate_with(&app, "zumar-core = {}", gen::Target::Live).cargo_toml;
        assert!(live.contains("default = []"));
        assert!(live.contains("wasm-bindgen = { version = \"0.2\", optional = true }"));
        assert!(!live.contains("[workspace]"));
        assert!(!live.contains("cdylib"));

        let wasm = gen::generate(&app, "zumar-core = {}").cargo_toml;
        assert!(wasm.contains("default = [\"wasm\"]"));
        assert!(wasm.contains("[workspace]"));
        assert!(wasm.contains("cdylib"));
    }

    #[test]
    fn counter_compiles() {
        let app = compile(COUNTER).unwrap();
        assert_eq!(app.name, "Counter");
        assert_eq!(app.msgs.len(), 3);
        assert_eq!(app.updates.len(), 3);
        assert_eq!(app.view.children.len(), 4);
    }

    #[test]
    fn counter_generates_expected_rust() {
        let rs = gen_rs(COUNTER);
        for needle in [
            "pub enum Msg {",
            "Msg::Inc => {",
            "let __new_count: i64 = (model.count + 1i64);",
            "model.count = __new_count;",
            ".on(\"click\", Msg::Dec)",
            ".child(text((model.count).to_string()))",
            "if (model.count > 9i64)",
            "zumar_runtime::zumar_app!(App, Model, Msg,",
        ] {
            assert!(rs.contains(needle), "missing {needle:?} in:\n{rs}");
        }
    }

    #[test]
    fn todo_compiles() {
        let app = compile(TODO).unwrap();
        assert_eq!(app.records.len(), 1);
        assert_eq!(app.msgs.len(), 5);
        // Draft carries a String payload; Add doesn't.
        assert_eq!(app.msgs[0].payload, Some(ast::Ty::Str));
        assert_eq!(app.msgs[1].payload, None);
    }

    #[test]
    fn todo_generates_records_payloads_and_comprehensions() {
        let rs = gen_rs(TODO);
        for needle in [
            "pub struct Item {",
            "Draft(String),",
            "Toggle(i64),",
            "Msg::Draft(s) =>",
            "Msg::Toggle(id) =>",
            // record literal + list concat in Add
            "Item { id: model.seq, text: model.draft.clone(), done: false }",
            // record update in Toggle
            "Item { done: (!t.done), ..t.clone() }",
            // comprehension lowers to a loop
            "__acc",
            "reverse",
            // list rendering + keyed child
            ".key((t.id).to_string())",
            ".on(\"click\", Msg::Toggle(t.id))",
        ] {
            assert!(rs.contains(needle), "missing {needle:?} in:\n{rs}");
        }
    }

    #[test]
    fn topic_sub_and_publish_cmd_lower_to_runtime_calls() {
        let src = r#"
app Chat
model { draft: String, last: String }
init = { draft = "", last = "" }
msg Draft String | Send | Got String
update Draft s = { draft = s }
update Send = { draft = "" } then publish("room", model.draft)
update Got s = { last = s }
sub = [ topic("room", Got) ]
view = div [] [ input [value model.draft, onInput Draft] [], button [onClick Send] [ text "send" ] ]
"#;
        let rs = gen_rs(src);
        assert!(
            rs.contains("zumar_runtime::publish(\"room\".to_string(), model.draft.clone())"),
            "{rs}"
        );
        assert!(
            rs.contains("zumar_runtime::topic(\"room\".to_string(), __topic_got)"),
            "{rs}"
        );
        assert!(rs.contains("fn __topic_got(msg: String) -> Msg {"), "{rs}");
    }

    #[test]
    fn topic_ctor_must_take_a_string() {
        let src = r#"
app X
model { n: Int }
init = { n = 0 }
msg Tick
update Tick = { n = model.n + 1 }
sub = [ topic("room", Tick) ]
view = div [] []
"#;
        let err = compile(src).unwrap_err();
        assert!(err.msg.contains("must take a String payload"), "{err}");
    }

    #[test]
    fn input_and_submit_events_lower_to_runtime_calls() {
        let src = r#"
app F
model { draft: String }
init = { draft = "" }
msg Draft String | Send
update Draft s = { draft = s }
update Send = { draft = "" }
view = form [onSubmit Send] [ input [value model.draft, onInput Draft] [] ]
"#;
        let rs = gen_rs(src);
        assert!(rs.contains(".on_submit(Msg::Send)"), "{rs}");
        assert!(rs.contains(".on_input(Msg::Draft)"), "{rs}");
        assert!(rs.contains(".attr(\"value\", model.draft.clone())"), "{rs}");
    }

    #[test]
    fn missing_update_equation_is_an_error() {
        let src = COUNTER.replace("update Reset = { count = 0 }", "");
        let err = compile(&src).unwrap_err();
        assert!(err.msg.contains("`Reset` has no `update"), "{err}");
    }

    #[test]
    fn unknown_field_is_an_error() {
        let err = compile(&COUNTER.replace("model.count + 1", "model.cuont + 1")).unwrap_err();
        assert!(err.msg.contains("no field `cuont`"), "{err}");
    }

    #[test]
    fn type_mismatch_is_an_error() {
        let err = compile(&COUNTER.replace("{ count = 0 }\n\nmsg", "{ count = \"zero\" }\n\nmsg"))
            .unwrap_err();
        assert!(err.msg.contains("expects Int"), "{err}");
    }

    #[test]
    fn unknown_onclick_msg_is_an_error() {
        let err = compile(&COUNTER.replace("onClick Reset", "onClick Rset")).unwrap_err();
        assert!(err.msg.contains("Rset"), "{err}");
    }

    #[test]
    fn concat_of_int_needs_show() {
        let src = COUNTER.replace("text show(model.count)", "text (\"n = \" ++ model.count)");
        let err = compile(&src).unwrap_err();
        assert!(err.msg.contains("`++` joins"), "{err}");
    }

    #[test]
    fn payload_must_be_bound() {
        let src = TODO.replace(
            "update Draft s = { draft = s }",
            "update Draft = { draft = \"\" }",
        );
        let err = compile(&src).unwrap_err();
        assert!(err.msg.contains("must bind the payload"), "{err}");
    }

    #[test]
    fn onclick_arg_type_is_checked() {
        // Toggle takes Int; feeding it a string must fail.
        let src = TODO.replace("onClick Toggle(t.id)", "onClick Toggle(t.text)");
        let err = compile(&src).unwrap_err();
        assert!(err.msg.contains("argument to `Toggle`"), "{err}");
    }

    #[test]
    fn oninput_ctor_must_take_string() {
        let src = TODO.replace(
            "span [onClick Toggle(t.id)] [ text t.text ]",
            "input [onInput Toggle] []",
        );
        let err = compile(&src).unwrap_err();
        assert!(err.msg.contains("Toggle"), "{err}");
    }

    #[test]
    fn comprehension_var_is_scoped() {
        // `t` is only in scope inside the comprehension.
        let src = TODO.replace(
            "update Reverse = { items = reverse(model.items) }",
            "update Reverse = { seq = t.id }",
        );
        let err = compile(&src).unwrap_err();
        assert!(
            err.msg.contains("`t` is not in scope") || err.msg.contains("unknown field"),
            "{err}"
        );
    }

    #[test]
    fn unknown_record_type_is_an_error() {
        let src = TODO.replace("items: List Item", "items: List Itm");
        let err = compile(&src).unwrap_err();
        assert!(err.msg.contains("unknown type `Itm`"), "{err}");
    }

    const EXPENSES: &str = r#"
app Expenses
record Item { id: Int, label: String, cents: Int }
model { label: String, amount: String, items: List Item, seq: Int }
init = { label = "", amount = "", items = [], seq = 1 }
msg Label String | Amount String | Add | Delete Int
update Label s = { label = s }
update Amount s = { amount = s }
update Add = {
  items = model.items ++ [{ id = model.seq, label = model.label, cents = toInt(model.amount) }],
  seq = model.seq + 1, label = "", amount = ""
}
update Delete id = { items = for t in model.items where t.id != id yield t }
view =
  div [] [
    span [] [ text (show(sum(for t in model.items yield t.cents)) ++ " total") ],
    ul [] [ for t in model.items { li [key show(t.id)] [ text t.label ] } ]
  ]
"#;

    #[test]
    fn expenses_compiles_with_sum_and_toint() {
        let rs = gen_rs(EXPENSES);
        assert!(rs.contains(".iter().sum::<i64>()"), "{rs}");
        assert!(
            rs.contains("model.amount.parse::<i64>().unwrap_or(0)"),
            "{rs}"
        );
    }

    #[test]
    fn sum_requires_list_int() {
        // sum over a List of records is a type error.
        let src = EXPENSES.replace(
            "sum(for t in model.items yield t.cents)",
            "sum(model.items)",
        );
        let err = compile(&src).unwrap_err();
        assert!(err.msg.contains("sum(..) takes a List Int"), "{err}");
    }

    #[test]
    fn nth_typechecks_and_lowers() {
        let src = r#"
app N
record Card { id: Int, face: String }
model { idx: Int, cards: List Card }
init = { idx = 0, cards = [] }
msg Next
update Next = { idx = model.idx + 1 }
view = div [] [ text (nth(model.cards, model.idx, { id = 0, face = "?" }).face) ]
"#;
        let rs = gen_rs(src);
        assert!(
            rs.contains(".get((model.idx) as usize).cloned().unwrap_or("),
            "{rs}"
        );
    }

    #[test]
    fn nth_default_type_is_checked() {
        let src = r#"
app N
model { xs: List Int, i: Int }
init = { xs = [], i = 0 }
msg M
update M = { i = nth(model.xs, model.i, "oops") }
view = div [] []
"#;
        let err = compile(src).unwrap_err();
        assert!(err.msg.contains("nth default"), "{err}");
    }

    #[test]
    fn toint_needs_a_string() {
        let src = r#"
app T
model { n: Int }
init = { n = 0 }
msg M
update M = { n = toInt(model.n) }
view = div [] []
"#;
        let err = compile(src).unwrap_err();
        assert!(err.msg.contains("toInt(..) takes a String"), "{err}");
    }

    const QUEUE: &str = r#"
app Queue
record Job { id: Int, name: String }
model { jobs: List Job, seq: Int }
init = { jobs = [], seq = 1 }
msg Add | Pop
update Add = { jobs = model.jobs ++ [{ id = model.seq, name = "job" }], seq = model.seq + 1 }
update Pop = { jobs = for j in model.jobs where j.id != model.seq yield j }
view =
  div [] [
    span [] [ text (case head(model.jobs) of none -> "empty" | some j -> j.name) ]
  ]
"#;

    #[test]
    fn maybe_and_case_compile_and_lower() {
        let rs = gen_rs(QUEUE);
        // head lowers to first().cloned(); case to a match with both arms.
        assert!(rs.contains(".first().cloned()"), "{rs}");
        assert!(rs.contains("match"), "{rs}");
        assert!(rs.contains("None =>"), "{rs}");
        assert!(rs.contains("Some(j) =>"), "{rs}");
    }

    #[test]
    fn maybe_field_becomes_option() {
        let src = r#"
app M
record C { id: Int, face: String }
model { pick: Maybe C }
init = { pick = none }
msg Set
update Set = { pick = some({ id = 1, face = "A" }) }
view = div [] [ text (case model.pick of none -> "?" | some c -> c.face) ]
"#;
        let rs = gen_rs(src);
        assert!(rs.contains("pub pick: Option<C>"), "{rs}");
        assert!(rs.contains("Some(C {"), "{rs}");
    }

    #[test]
    fn case_arms_must_agree() {
        let src = QUEUE.replace(
            "case head(model.jobs) of none -> \"empty\" | some j -> j.name",
            "case head(model.jobs) of none -> 0 | some j -> j.name",
        );
        let err = compile(&src).unwrap_err();
        assert!(err.msg.contains("`case` arms disagree"), "{err}");
    }

    #[test]
    fn case_needs_a_maybe() {
        let src = QUEUE.replace("case head(model.jobs)", "case model.seq");
        let err = compile(&src).unwrap_err();
        assert!(err.msg.contains("scrutinee must be a Maybe"), "{err}");
    }

    #[test]
    fn bare_none_needs_context() {
        // `none` as its own scrutinee has nothing to fix its element type.
        let src = r#"
app N
model { x: Int }
init = { x = 0 }
msg M
update M = { x = case none of none -> 0 | some y -> y }
view = div [] []
"#;
        let err = compile(src).unwrap_err();
        assert!(err.msg.contains("what `none` is"), "{err}");
    }

    const CLOCK: &str = r#"
app Clock
model { now: Int, running: Bool, quote: String, pinged: String }
init = { now = 0, running = true, quote = "loading", pinged = "" } then httpGet("./quote.txt", Got)
msg Tick Int | Toggle | Refetch | Got String | Ping | Pong
update Tick t = { now = t }
update Toggle = { running = not model.running }
update Refetch = { quote = "..." } then httpGet("./quote.txt", Got)
update Got s = { quote = s }
update Ping = { pinged = "ping..." } then delay(1500, Pong)
update Pong = { pinged = "pong!" }
sub = if model.running then [ every(1000, Tick) ] else []
view =
  div [] [
    span [class "sec"] [ text show((model.now / 1000) % 60) ],
    button [onClick Toggle] [ text (if model.running then "stop" else "start") ],
    p [class "q"] [ text model.quote ],
    button [onClick Refetch] [ text "refetch" ],
    button [onClick Ping] [ text "ping" ],
    span [] [ text model.pinged ]
  ]
"#;

    #[test]
    fn clock_effects_compile_and_lower() {
        let rs = gen_rs(CLOCK);
        for needle in [
            // commands from update arms + init
            // cmds bind before the field assignments (pre-update model), then return
            "let __cmds = vec![zumar_runtime::http_get(\"./quote.txt\".to_string(), __http_got)];",
            "let __cmds = vec![zumar_runtime::delay(1500, Msg::Pong)];",
            "return __cmds;",
            ".with_init(vec![zumar_runtime::http_get(",
            // http callback fn: body on ok, error text otherwise
            "fn __http_got(r: zumar_runtime::effects::HttpResult) -> Msg {",
            // clocked subscription with model-driven lifecycle
            "fn __tick_tick(now: f64) -> Msg {",
            "zumar_runtime::every_with_now(1000, __tick_tick)",
            ".with_subscriptions(subs)",
            "if model.running { vec![zumar_runtime::every_with_now(1000, __tick_tick)] } else { vec![] }",
            // div/rem lower checked
            ".checked_div(1000i64).unwrap_or(0)",
            ".checked_rem(60i64).unwrap_or(0)",
        ] {
            assert!(rs.contains(needle), "missing {needle:?} in:\n{rs}");
        }
    }

    #[test]
    fn delay_msg_must_be_payload_less() {
        let src = CLOCK.replace("delay(1500, Pong)", "delay(1500, Got)");
        let err = compile(&src).unwrap_err();
        assert!(err.msg.contains("delay fires `Got`"), "{err}");
    }

    #[test]
    fn httpget_ctor_must_take_string() {
        let src = CLOCK.replace(
            "httpGet(\"./quote.txt\", Got)",
            "httpGet(\"./quote.txt\", Pong)",
        );
        let err = compile(&src).unwrap_err();
        assert!(err.msg.contains("must take a String payload"), "{err}");
    }

    #[test]
    fn every_msg_payload_is_none_or_int() {
        let src = CLOCK.replace("every(1000, Tick)", "every(1000, Got)");
        let err = compile(&src).unwrap_err();
        assert!(err.msg.contains("takes a String"), "{err}");
    }

    #[test]
    fn sub_condition_must_be_bool() {
        let src = CLOCK.replace("if model.running then", "if model.now then");
        let err = compile(&src).unwrap_err();
        assert!(err.msg.contains("`sub` condition must be Bool"), "{err}");
    }

    #[test]
    fn division_by_zero_is_zero_not_panic() {
        let src = r#"
app D
model { n: Int }
init = { n = 0 }
msg M
update M = { n = (10 / model.n) + (10 % model.n) }
view = div [] [ text show(model.n) ]
"#;
        let rs = gen_rs(src);
        assert!(rs.contains("checked_div"), "{rs}");
        assert!(rs.contains("checked_rem"), "{rs}");
    }

    const KANBAN: &str = r#"
app Kanban
enum Status = Todo | Doing | Done
record Task { id: Int, name: String, status: Status }
model { draft: String, tasks: List Task, seq: Int }
init = { draft = "", tasks = [], seq = 1 }
msg Draft String | Add | Advance Int
update Draft s = { draft = s }
update Add = {
  tasks = model.tasks ++ [{ id = model.seq, name = model.draft, status = Todo }],
  seq = model.seq + 1, draft = ""
}
update Advance id = {
  tasks = for t in model.tasks yield
    (if t.id == id
     then { t | status = case t.status of Todo -> Doing | Doing -> Done | Done -> Todo }
     else t)
}
view =
  div [] [
    span [] [ text show(length(for t in model.tasks where t.status == Doing yield t)) ],
    ul [] [
      for t in model.tasks {
        li [class (case t.status of Todo -> "todo" | Doing -> "doing" | Done -> "done")] [
          span [onClick Advance(t.id)] [ text t.name ]
        ]
      }
    ]
  ]
"#;

    #[test]
    fn enums_compile_and_lower_to_rust_enums() {
        let rs = gen_rs(KANBAN);
        for needle in [
            "#[derive(Clone, PartialEq)]\npub enum Status {",
            "    Todo,\n    Doing,\n    Done,",
            "status: Status,",
            "status: Status::Todo",
            "match t.status.clone() { Status::Todo => { Status::Doing } Status::Doing => { Status::Done } Status::Done => { Status::Todo } }",
            "(t.status.clone() == Status::Doing)",
        ] {
            assert!(rs.contains(needle), "missing {needle:?} in:\n{rs}");
        }
    }

    #[test]
    fn case_over_enum_must_cover_every_variant() {
        let src = KANBAN.replace("| Done -> Todo }", "}");
        let err = compile(&src).unwrap_err();
        assert!(err.msg.contains("missing variant `Done`"), "{err}");
    }

    #[test]
    fn unknown_variant_in_case_is_an_error() {
        let src = KANBAN.replace("Todo -> Doing |", "Blocked -> Doing |");
        let err = compile(&src).unwrap_err();
        assert!(err.msg.contains("not a variant of Status"), "{err}");
    }

    #[test]
    fn duplicate_case_arm_is_an_error() {
        let src = KANBAN.replace("Doing -> Done |", "Todo -> Done |");
        let err = compile(&src).unwrap_err();
        assert!(err.msg.contains("duplicate `Todo` arm"), "{err}");
    }

    #[test]
    fn enum_payload_variants_bind_in_case() {
        let src = r#"
app P
enum Filter = All | ByOwner String
model { f: Filter, who: String }
init = { f = All, who = "" }
msg Set String
update Set s = { f = ByOwner(s), who = case ByOwner(s) of All -> "" | ByOwner o -> o }
view = div [] [ text model.who ]
"#;
        let rs = gen_rs(src);
        assert!(rs.contains("ByOwner(String),"), "{rs}");
        assert!(rs.contains("Filter::ByOwner(o) => { o.clone() }"), "{rs}");
    }

    #[test]
    fn payload_variant_needs_binder() {
        let src = r#"
app P
enum F = A | B Int
model { n: Int }
init = { n = 0 }
msg M
update M = { n = case B(1) of A -> 0 | B -> 1 }
view = div [] []
"#;
        let err = compile(src).unwrap_err();
        assert!(err.msg.contains("bind it"), "{err}");
    }

    #[test]
    fn eq_on_payload_enum_is_an_error() {
        let src = r#"
app P
enum F = A | B Int
model { f: F, ok: Bool }
init = { f = A, ok = false }
msg M
update M = { ok = model.f == A }
view = div [] []
"#;
        let err = compile(src).unwrap_err();
        assert!(err.msg.contains("plain enums"), "{err}");
    }

    #[test]
    fn variant_names_share_one_namespace() {
        let src = r#"
app P
enum A = X | Y
enum B = Y | Z
model { n: Int }
init = { n = 0 }
msg M
update M = { n = 0 }
view = div [] []
"#;
        let err = compile(src).unwrap_err();
        assert!(err.msg.contains("already declared"), "{err}");
    }

    #[test]
    fn fold_lowers_on_the_rust_backend() {
        let src = r#"
app F
record Item { id: Int, cents: Int }
model { items: List Item }
init = { items = [] }
msg M
update M = { items = [] }
view = div [] [ text show(fold(model.items, 0, acc t -> acc + t.cents)) ]
"#;
        let rs = gen_rs(src);
        assert!(rs.contains("let mut __f"), "{rs}");
        assert!(rs.contains("for t in (model.items.clone()).iter()"), "{rs}");
        assert!(rs.contains("let acc = __f"), "{rs}");
    }

    #[test]
    fn fold_body_must_match_init_type() {
        let src = r#"
app F
record Item { id: Int }
model { items: List Item, n: Int }
init = { items = [], n = 0 }
msg M
update M = { n = fold(model.items, 0, acc t -> "nope") }
view = div [] []
"#;
        let err = compile(src).unwrap_err();
        assert!(err.msg.contains("fold body"), "{err}");
    }

    #[test]
    fn pathological_nesting_errors_cleanly() {
        let bomb = format!(
            "app X\nmodel {{ a: Int }}\ninit = {{ a = {}1{} }}\nmsg M\nupdate M = {{ a = 1 }}\nview = div [] []",
            "(".repeat(5000),
            ")".repeat(5000)
        );
        let err = compile(&bomb).unwrap_err();
        assert!(err.msg.contains("nesting deeper"), "{err}");
    }

    #[test]
    fn error_positions_are_line_accurate() {
        let err = compile("app X\nmodel { a: Int }\ninit = { a = 0 }\nmsg M\nupdate M = { b = 1 }\nview = div [] []").unwrap_err();
        assert_eq!(err.line, 5);
        assert!(err.msg.contains("unknown field `b`"));
    }

    #[test]
    fn compile_with_merges_shared_decls() {
        let decls = parse_decls(
            "record Todo { id: Int, title: String, done: Bool }\nenum Level = Low | High",
        )
        .unwrap();
        let app = compile_with(
            "app X\nmodel { items: List Todo }\ninit = { items = [] }\nmsg M\nupdate M = { items = model.items }\nview = div [] []",
            decls,
        )
        .unwrap();
        assert_eq!(app.records.len(), 1);
        assert_eq!(app.enums.len(), 1);
    }

    #[test]
    fn renamed_schema_field_is_a_frontend_compile_error() {
        // The P2 payoff: user code compiled against yesterday's schema fails
        // the moment the generated decls change underneath it.
        let src = "app X\nmodel { items: List Todo }\ninit = { items = [ { id = 1, title = \"x\" } ] }\nmsg M\nupdate M = { items = model.items }\nview = div [] []";
        let old = parse_decls("record Todo { id: Int, title: String }").unwrap();
        assert!(compile_with(src, old).is_ok());
        let renamed = parse_decls("record Todo { id: Int, name: String }").unwrap();
        assert!(compile_with(src, renamed).is_err());
    }

    #[test]
    fn decls_fragments_reject_program_declarations_and_duplicates() {
        assert!(parse_decls("model { a: Int }").is_err());
        assert!(parse_decls("record A { x: Int }\nenum B = C | D").is_ok());
        // a fragment record colliding with a program record is the usual
        // duplicate error
        let decls = parse_decls("record Todo { id: Int }").unwrap();
        let err = compile_with(
            "app X\nrecord Todo { id: Int }\nmodel { a: Int }\ninit = { a = 0 }\nmsg M\nupdate M = { a = 0 }\nview = div [] []",
            decls,
        )
        .unwrap_err();
        assert!(err.msg.contains("duplicate record"), "{err}");
    }
}
