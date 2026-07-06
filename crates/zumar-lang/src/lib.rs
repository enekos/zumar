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

/// Parse + typecheck.
pub fn compile(source: &str) -> Result<App, ZuError> {
    let app = parse::parse(source)?;
    check::check(&app)?;
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
}
