#![forbid(unsafe_code)]
//! zumar-lang v0 — an Elm-like language compiling to zumar TEA apps.
//!
//! Pipeline: [`parse`] → [`check`] → [`generate`]. The frontend is
//! backend-agnostic; today's backend emits a Rust crate built with the
//! existing wasm toolchain, and a WasmGC-direct backend is planned behind
//! the same AST (the reason zumar was built runtime-first).
//!
//! v0 scope: one model record (Int/String/Bool fields), payload-less
//! messages, per-message update equations with record-update semantics,
//! expressions with arithmetic/`++`/comparisons/`if`/`show`, and an
//! element tree with string attributes and `onClick`. Total by
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
        let app = compile(COUNTER).unwrap();
        let generated = gen::generate(&app, "../..");
        assert_eq!(generated.crate_name, "counter");
        for needle in [
            "pub enum Msg {",
            "Msg::Inc => {",
            "let __new_count: i64 = (model.count + 1);",
            "model.count = __new_count;",
            ".on(\"click\", Msg::Dec)",
            ".child(text((model.count).to_string()))",
            "if (model.count > 9) { \"double digits!\".to_string() }",
            "zumar_runtime::zumar_app!(App, Model, Msg,",
        ] {
            assert!(generated.lib_rs.contains(needle), "missing {needle:?} in:\n{}", generated.lib_rs);
        }
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
        assert!(err.msg.contains("`onClick Rset`"), "{err}");
    }

    #[test]
    fn concat_of_int_needs_show() {
        let src = COUNTER.replace("text show(model.count)", "text (\"n = \" ++ model.count)");
        let err = compile(&src).unwrap_err();
        assert!(err.msg.contains("`++` joins Strings"), "{err}");
    }

    #[test]
    fn pathological_nesting_errors_cleanly() {
        // 5000 nested parens must not overflow the compiler stack.
        let bomb = format!(
            "app X\nmodel {{ a: Int }}\ninit = {{ a = {}1{} }}\nmsg M\nupdate M = {{ a = 1 }}\nview = div [] []",
            "(".repeat(5000),
            ")".repeat(5000)
        );
        let err = compile(&bomb).unwrap_err();
        assert!(err.msg.contains("nesting deeper"), "{err}");

        // Same for element nesting in view.
        let deep_view = format!(
            "app X\nmodel {{ a: Int }}\ninit = {{ a = 0 }}\nmsg M\nupdate M = {{ a = 1 }}\nview = {}div [] []{}",
            "div [] [ ".repeat(5000),
            " ]".repeat(5000)
        );
        let err = compile(&deep_view).unwrap_err();
        assert!(err.msg.contains("nesting deeper"), "{err}");
    }

    #[test]
    fn error_positions_are_line_accurate() {
        let err = compile("app X\nmodel { a: Int }\ninit = { a = 0 }\nmsg M\nupdate M = { b = 1 }\nview = div [] []").unwrap_err();
        assert_eq!(err.line, 5);
        assert!(err.msg.contains("unknown field `b`"));
    }
}
