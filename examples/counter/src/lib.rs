//! Counter example. Note what's *absent*: no web-sys, no DOM types, no
//! element references. The app is pure model/update/view; the only Wasm
//! surface is two JSON-returning methods.

use wasm_bindgen::prelude::*;

use zumar_core::{el, text, VNode};
use zumar_runtime::Program;

#[derive(Clone)]
enum Msg {
    Inc,
    Dec,
    Reset,
}

struct Model {
    count: i32,
}

fn update(model: &mut Model, msg: Msg) {
    match msg {
        Msg::Inc => model.count += 1,
        Msg::Dec => model.count -= 1,
        Msg::Reset => model.count = 0,
    }
}

fn view(model: &Model) -> VNode<Msg> {
    el("div")
        .attr("class", "counter")
        .child(el("h1").text("zumar"))
        .child(
            el("p")
                .attr("class", "sub")
                .text("the Elm architecture over a Wasm patch protocol"),
        )
        .child(
            el("div")
                .attr("class", "row")
                .child(el("button").on("click", Msg::Dec).text("−"))
                .child(
                    el("span")
                        .attr("class", if model.count < 0 { "count neg" } else { "count" })
                        .text(model.count.to_string()),
                )
                .child(el("button").on("click", Msg::Inc).text("+")),
        )
        .child(el("button").attr("class", "reset").on("click", Msg::Reset).text("reset"))
        .child(if model.count.abs() >= 10 {
            el("p").attr("class", "note").child(text("that's a lot of clicks"))
        } else {
            el("p").attr("class", "note hidden")
        })
        .into()
}

#[wasm_bindgen]
pub struct App {
    program: Program<Model, Msg>,
}

#[wasm_bindgen]
impl App {
    #[wasm_bindgen(constructor)]
    pub fn new() -> App {
        App {
            program: Program::new(Model { count: 0 }, update, view),
        }
    }

    /// JSON `{ root, events }` — the full initial tree.
    pub fn init(&self) -> String {
        serde_json::to_string(&self.program.initial_render()).unwrap()
    }

    /// JSON `{ patches, events }` for one DOM event at `path`.
    pub fn dispatch(&mut self, path: Vec<u32>, event: String, payload: String) -> String {
        let payload = serde_json::from_str(&payload).unwrap_or_default();
        serde_json::to_string(&self.program.dispatch(&path, &event, &payload)).unwrap()
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}
