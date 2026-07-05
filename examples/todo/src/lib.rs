//! Todo example — exercises everything M2 added: controlled text input
//! (`on_input` + value round-trip), form submit with preventDefault, keyed
//! list diffing (toggle/delete/reverse produce moves and inserts, not
//! rebuilds), and checkbox state.

use wasm_bindgen::prelude::*;

use zumar_core::{el, VNode};
use zumar_runtime::Program;

#[derive(Clone)]
enum Msg {
    Draft(String),
    Add,
    Toggle(u32),
    Delete(u32),
    Reverse,
}

struct Todo {
    id: u32,
    text: String,
    done: bool,
}

struct Model {
    draft: String,
    todos: Vec<Todo>,
    next_id: u32,
}

fn update(model: &mut Model, msg: Msg) {
    match msg {
        Msg::Draft(s) => model.draft = s,
        Msg::Add => {
            let text = model.draft.trim().to_string();
            if !text.is_empty() {
                model.todos.push(Todo { id: model.next_id, text, done: false });
                model.next_id += 1;
                model.draft.clear();
            }
        }
        Msg::Toggle(id) => {
            if let Some(t) = model.todos.iter_mut().find(|t| t.id == id) {
                t.done = !t.done;
            }
        }
        Msg::Delete(id) => model.todos.retain(|t| t.id != id),
        Msg::Reverse => model.todos.reverse(),
    }
}

fn view(model: &Model) -> VNode<Msg> {
    let open = model.todos.iter().filter(|t| !t.done).count();

    let mut list = el("ul").attr("class", "todos");
    for t in &model.todos {
        list = list.child(
            el("li")
                .key(t.id.to_string())
                .attr("class", if t.done { "done" } else { "open" })
                .child(
                    el("input")
                        .attr("type", "checkbox")
                        .attr_if(t.done, "checked", "checked")
                        .on("change", Msg::Toggle(t.id)),
                )
                .child(el("span").text(t.text.clone()))
                .child(el("button").attr("class", "del").on("click", Msg::Delete(t.id)).text("×")),
        );
    }

    el("div")
        .attr("class", "todo")
        .child(el("h1").text("zumar todo"))
        .child(
            el("form")
                .on_submit(Msg::Add)
                .child(
                    el("input")
                        .attr("type", "text")
                        .attr("placeholder", "what needs doing?")
                        .attr("value", model.draft.clone())
                        .on_input(Msg::Draft),
                )
                .child(el("button").attr("type", "submit").text("add")),
        )
        .child(
            el("div")
                .attr("class", "bar")
                .child(el("span").text(format!("{open} open")))
                .child(el("button").on("click", Msg::Reverse).text("reverse")),
        )
        .child(list)
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
            program: Program::new(
                Model { draft: String::new(), todos: Vec::new(), next_id: 1 },
                update,
                view,
            ),
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
