//! counter.zu's zuc-generated Rust backend output (examples/lang-counter),
//! verbatim except: the `zumar_app!` wasm-bindgen wrapper is replaced by
//! `program()` and the wasm-bindgen import is dropped. Live mode consumes
//! the same generated update/view the client build does — zero new codegen.

use zumar_core::{el, text, VNode};
use zumar_runtime::{effects::Cmds, Program};

#[derive(Clone)]
pub enum Msg {
    Inc,
    Dec,
    Reset,
}

pub struct Model {
    pub count: i64,
}

fn init_model() -> Model {
    Model { count: 0 }
}

fn update(model: &mut Model, msg: Msg) -> Cmds<Msg> {
    match msg {
        Msg::Inc => {
            let __new_count: i64 = model.count + 1;
            model.count = __new_count;
        }
        Msg::Dec => {
            let __new_count: i64 = model.count - 1;
            model.count = __new_count;
        }
        Msg::Reset => {
            let __new_count: i64 = 0;
            model.count = __new_count;
        }
    }
    Vec::new()
}

#[allow(unused_variables)]
fn view(model: &Model) -> VNode<Msg> {
    el("div")
        .attr("class", "counter".to_string())
        .child(el("h1").child(text("zumar-lang".to_string())))
        .child(
            el("p")
                .attr("class", "sub".to_string())
                .child(text("this page was compiled from counter.zu".to_string())),
        )
        .child(
            el("div")
                .attr("class", "row".to_string())
                .child(
                    el("button")
                        .on("click", Msg::Dec)
                        .child(text("-".to_string())),
                )
                .child(
                    el("span")
                        .attr("class", "count".to_string())
                        .child(text((model.count).to_string())),
                )
                .child(
                    el("button")
                        .on("click", Msg::Inc)
                        .child(text("+".to_string())),
                ),
        )
        .child(
            el("button")
                .attr("class", "reset".to_string())
                .on("click", Msg::Reset)
                .child(text("reset".to_string())),
        )
        .child(
            el("p")
                .attr("class", "note".to_string())
                .child(text(if model.count > 9 {
                    "double digits!".to_string()
                } else if (0 - 9) > model.count {
                    "very negative!".to_string()
                } else {
                    "".to_string()
                })),
        )
        .into()
}

pub fn program() -> Program<Model, Msg> {
    Program::new(init_model(), update, view)
}
