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

use zumar_core::{collect_events, diff, find_handler, Patch, SerNode, VNode};

pub struct Program<Model, Msg> {
    model: Model,
    update: fn(&mut Model, Msg),
    view: fn(&Model) -> VNode<Msg>,
    current: VNode<Msg>,
}

/// First render: the full tree plus the event names the shim must delegate.
#[derive(Debug, Serialize)]
pub struct InitialRender {
    pub root: SerNode,
    pub events: Vec<String>,
}

/// Result of one dispatch: patches to apply, and the (possibly changed)
/// set of event names to delegate.
#[derive(Debug, Serialize)]
pub struct Update {
    pub patches: Vec<Patch>,
    pub events: Vec<String>,
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

    /// Handle one DOM event. A path with no matching handler (event raced a
    /// render, or fired on a handler-free subtree) is a no-op, not an error.
    pub fn dispatch(&mut self, path: &[u32], event: &str) -> Update {
        let Some(msg) = find_handler(&self.current, path, event).cloned() else {
            return Update { patches: Vec::new(), events: Vec::new() };
        };
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
    use zumar_core::{el, text};

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

        let init = program.initial_render();
        assert_eq!(init.events, vec!["click"]);

        // Click the "+" button (third child); event target is its text node.
        let up = program.dispatch(&[2, 0], "click");
        assert_eq!(*program.model(), 1);
        assert_eq!(
            up.patches,
            vec![Patch::SetText { path: vec![1, 0], text: "1".into() }]
        );

        // Click "-" on the button element itself.
        let down = program.dispatch(&[0], "click");
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
        let up = program.dispatch(&[1], "mouseover");
        assert!(up.patches.is_empty());
        assert_eq!(*program.model(), 0);
    }

    #[test]
    fn events_bubble_to_nearest_ancestor_handler() {
        let mut program = Program::new(0, update, view);
        // Path deep inside the "+" button's text node still finds the
        // button's handler.
        program.dispatch(&[2, 0], "click");
        assert_eq!(*program.model(), 1);
    }
}
