//! zumar-runtime — the Elm-architecture loop over the zumar patch protocol.
//!
//! A [`Program`] owns the model and the current vdom. The host (Wasm glue +
//! JS shim) drives it with exactly two calls:
//!
//! - [`Program::initial_render`] once, to materialize the tree;
//! - [`Program::dispatch`] per DOM event, with the event's node path and
//!   name. The program resolves the handler vdom-side, runs `update`,
//!   re-renders, and returns the diff.
//!
//! Nothing here knows about wasm-bindgen or the DOM, so the whole loop is
//! testable natively.

use serde::Serialize;

use zumar_core::{
    collect_events, diff, find_listener, EventPayload, EventSpec, Patch, SerNode, VNode,
};

pub struct Program<Model, Msg> {
    model: Model,
    update: fn(&mut Model, Msg),
    view: fn(&Model) -> VNode<Msg>,
    current: VNode<Msg>,
}

/// First render: the full tree plus the event specs the shim must delegate.
#[derive(Debug, Serialize)]
pub struct InitialRender {
    pub root: SerNode,
    pub events: Vec<EventSpec>,
}

/// Result of one dispatch: patches to apply, and the (possibly changed)
/// set of event specs to delegate.
#[derive(Debug, Serialize)]
pub struct Update {
    pub patches: Vec<Patch>,
    pub events: Vec<EventSpec>,
}

impl<Model, Msg: Clone> Program<Model, Msg> {
    pub fn new(model: Model, update: fn(&mut Model, Msg), view: fn(&Model) -> VNode<Msg>) -> Self {
        let current = view(&model);
        Program { model, update, view, current }
    }

    pub fn initial_render(&self) -> InitialRender {
        InitialRender {
            root: SerNode::from_vnode(&self.current),
            events: collect_events(&self.current),
        }
    }

    /// Handle one DOM event. A path with no matching listener (event raced
    /// a render, or fired on a handler-free subtree) is a no-op, not an
    /// error. `payload` is the standard envelope the shim extracted; the
    /// listener's handler decides which field, if any, it consumes.
    pub fn dispatch(&mut self, path: &[u32], event: &str, payload: &EventPayload) -> Update {
        let Some(listener) = find_listener(&self.current, path, event) else {
            return Update { patches: Vec::new(), events: Vec::new() };
        };
        let msg = listener.handler.resolve(payload);
        (self.update)(&mut self.model, msg);
        let new = (self.view)(&self.model);
        let patches = diff(&self.current, &new);
        let events = collect_events(&new);
        self.current = new;
        Update { patches, events }
    }

    pub fn model(&self) -> &Model {
        &self.model
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zumar_core::el;

    #[derive(Clone, PartialEq, Debug)]
    enum Msg {
        Inc,
        Dec,
    }

    fn update(model: &mut i32, msg: Msg) {
        match msg {
            Msg::Inc => *model += 1,
            Msg::Dec => *model -= 1,
        }
    }

    fn view(model: &i32) -> VNode<Msg> {
        el("div")
            .child(el("button").on("click", Msg::Dec).text("-"))
            .child(el("span").text(model.to_string()))
            .child(el("button").on("click", Msg::Inc).text("+"))
            .into()
    }

    #[test]
    fn full_loop_counter() {
        let mut program = Program::new(0, update, view);
        let none = EventPayload::default();

        let init = program.initial_render();
        assert_eq!(init.events.len(), 1);
        assert_eq!(init.events[0].name, "click");
        assert!(!init.events[0].prevent_default);

        // Click the "+" button (third child); event target is its text node.
        let up = program.dispatch(&[2, 0], "click", &none);
        assert_eq!(*program.model(), 1);
        assert_eq!(
            up.patches,
            vec![Patch::SetText { path: vec![1, 0], text: "1".into() }]
        );

        // Click "-" on the button element itself.
        let down = program.dispatch(&[0], "click", &none);
        assert_eq!(*program.model(), 0);
        assert_eq!(
            down.patches,
            vec![Patch::SetText { path: vec![1, 0], text: "0".into() }]
        );
    }

    #[test]
    fn dispatch_without_handler_is_noop() {
        let mut program = Program::new(0, update, view);
        // The span has no click handler and neither does the root div.
        let up = program.dispatch(&[1], "mouseover", &EventPayload::default());
        assert!(up.patches.is_empty());
        assert_eq!(*program.model(), 0);
    }

    #[test]
    fn events_bubble_to_nearest_ancestor_handler() {
        let mut program = Program::new(0, update, view);
        // Path deep inside the "+" button's text node still finds the
        // button's handler.
        program.dispatch(&[2, 0], "click", &EventPayload::default());
        assert_eq!(*program.model(), 1);
    }

    #[derive(Clone, PartialEq, Debug)]
    enum FormMsg {
        Draft(String),
        Submit,
    }

    fn form_update(model: &mut String, msg: FormMsg) {
        match msg {
            FormMsg::Draft(s) => *model = s,
            FormMsg::Submit => model.push('!'),
        }
    }

    #[allow(clippy::ptr_arg)] // Model = String, and view must be fn(&Model)
    fn form_view(model: &String) -> VNode<FormMsg> {
        el("form")
            .on_submit(FormMsg::Submit)
            .child(el("input").attr("value", model.clone()).on_input(FormMsg::Draft))
            .into()
    }

    #[test]
    fn payload_value_flows_into_message() {
        let mut program = Program::new(String::new(), form_update, form_view);
        let typed = EventPayload { value: Some("hej".into()), ..Default::default() };
        let up = program.dispatch(&[0], "input", &typed);
        assert_eq!(program.model(), "hej");
        // The controlled input's value attr round-trips through the diff.
        assert_eq!(
            up.patches,
            vec![Patch::SetAttr { path: vec![0], name: "value".into(), value: "hej".into() }]
        );
    }

    #[test]
    fn submit_requests_prevent_default() {
        let program = Program::new(String::new(), form_update, form_view);
        let init = program.initial_render();
        let submit = init.events.iter().find(|s| s.name == "submit").unwrap();
        let input = init.events.iter().find(|s| s.name == "input").unwrap();
        assert!(submit.prevent_default);
        assert!(!input.prevent_default);
    }
}
