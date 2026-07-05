#![forbid(unsafe_code)]
//! zumar-runtime — the Elm-architecture loop over the zumar patch protocol.
//!
//! A [`Program`] owns the model, the current vdom, and the effect ledgers
//! (pending commands, active subscriptions). The host (Wasm glue + JS shim)
//! drives it with four calls, all returning the same shape:
//!
//! - [`Program::initial_render`] once, to materialize the tree;
//! - [`Program::dispatch`] per DOM event;
//! - [`Program::resolve`] when a one-shot command completes;
//! - [`Program::notify`] each time a subscription fires.
//!
//! Nothing here knows about wasm-bindgen or the DOM, so the whole loop —
//! including the full effects lifecycle — is testable natively.

pub mod effects;
pub mod wire;

use std::collections::BTreeMap;

use serde::Serialize;

use zumar_core::{
    collect_events, diff, find_listener, EventPayload, EventSpec, Patch, SerNode, VNode,
};

pub use effects::{delay, every, every_with_now, http_get};
use effects::{Cmd, CmdCallback, CmdOut, Cmds, FxPayload, HttpResult, Sub, SubCallback, SubDelta};
pub use zumar_core::EventPayload as WireEventPayload;

/// Generate the wasm-bindgen app wrapper: constructor + the four boundary
/// calls, all returning wire-encoded bytes. Payloads arrive as explicit
/// scalars, so no JSON crosses the boundary in either direction.
///
/// The invoking crate must have `wasm_bindgen::prelude::*` in scope and
/// depend on `wasm-bindgen`.
///
/// ```ignore
/// zumar_app!(App, Model, Msg, {
///     Program::new(Model::default(), update, view).with_subscriptions(subs)
/// });
/// ```
#[macro_export]
macro_rules! zumar_app {
    ($app:ident, $model:ty, $msg:ty, $program:expr) => {
        #[wasm_bindgen]
        pub struct $app {
            program: $crate::Program<$model, $msg>,
        }

        #[wasm_bindgen]
        impl $app {
            #[wasm_bindgen(constructor)]
            pub fn new() -> $app {
                $app { program: $program }
            }

            pub fn init(&mut self) -> Vec<u8> {
                self.program.initial_render().to_bytes()
            }

            pub fn dispatch(
                &mut self,
                path: Vec<u32>,
                event: String,
                value: Option<String>,
                checked: Option<bool>,
                key: Option<String>,
            ) -> Vec<u8> {
                let payload = $crate::WireEventPayload {
                    value,
                    checked,
                    key,
                };
                self.program.dispatch(&path, &event, &payload).to_bytes()
            }

            pub fn resolve(
                &mut self,
                id: u32,
                ok: Option<bool>,
                status: Option<u16>,
                body: Option<String>,
            ) -> Vec<u8> {
                let payload = $crate::effects::FxPayload {
                    ok,
                    status,
                    body,
                    now: None,
                };
                self.program.resolve(id, &payload).to_bytes()
            }

            pub fn notify(&mut self, id: u32, now: Option<f64>) -> Vec<u8> {
                let payload = $crate::effects::FxPayload {
                    ok: None,
                    status: None,
                    body: None,
                    now,
                };
                self.program.notify(id, &payload).to_bytes()
            }
        }

        impl Default for $app {
            fn default() -> Self {
                Self::new()
            }
        }
    };
}

pub struct Program<Model, Msg> {
    model: Model,
    update: fn(&mut Model, Msg) -> Cmds<Msg>,
    view: fn(&Model) -> VNode<Msg>,
    subscriptions: fn(&Model) -> Vec<Sub<Msg>>,
    current: VNode<Msg>,
    init_cmds: Cmds<Msg>,
    pending: BTreeMap<u32, CmdCallback<Msg>>,
    active_subs: BTreeMap<String, ActiveSub<Msg>>,
    next_id: u32,
}

struct ActiveSub<Msg> {
    id: u32,
    callback: SubCallback<Msg>,
}

/// First render: the full tree, the event specs the shim must delegate,
/// plus any init-time commands and subscription starts.
#[derive(Debug, Serialize)]
pub struct InitialRender {
    pub root: SerNode,
    pub events: Vec<EventSpec>,
    pub cmds: Vec<CmdOut>,
    pub subs: Vec<SubDelta>,
}

/// Result of one program step (dispatch/resolve/notify): patches to apply,
/// the current event specs, commands to execute, subscription changes.
#[derive(Debug, Serialize)]
pub struct Update {
    pub patches: Vec<Patch>,
    pub events: Vec<EventSpec>,
    pub cmds: Vec<CmdOut>,
    pub subs: Vec<SubDelta>,
}

impl Update {
    fn noop() -> Update {
        Update {
            patches: Vec::new(),
            events: Vec::new(),
            cmds: Vec::new(),
            subs: Vec::new(),
        }
    }
}

fn no_subs<Model, Msg>(_: &Model) -> Vec<Sub<Msg>> {
    Vec::new()
}

impl<Model, Msg: Clone> Program<Model, Msg> {
    pub fn new(
        model: Model,
        update: fn(&mut Model, Msg) -> Cmds<Msg>,
        view: fn(&Model) -> VNode<Msg>,
    ) -> Self {
        let current = view(&model);
        Program {
            model,
            update,
            view,
            subscriptions: no_subs::<Model, Msg>,
            current,
            init_cmds: Vec::new(),
            pending: BTreeMap::new(),
            active_subs: BTreeMap::new(),
            next_id: 1,
        }
    }

    pub fn with_subscriptions(mut self, subs: fn(&Model) -> Vec<Sub<Msg>>) -> Self {
        self.subscriptions = subs;
        self
    }

    /// Commands to execute right after the initial render (Elm's `init`
    /// commands) — e.g. a first HTTP fetch.
    pub fn with_init(mut self, cmds: Cmds<Msg>) -> Self {
        self.init_cmds = cmds;
        self
    }

    pub fn initial_render(&mut self) -> InitialRender {
        let init_cmds = std::mem::take(&mut self.init_cmds);
        InitialRender {
            root: SerNode::from_vnode(&self.current),
            events: collect_events(&self.current),
            cmds: self.register_cmds(init_cmds),
            subs: self.diff_subs(),
        }
    }

    /// Handle one DOM event. A path with no matching listener (event raced
    /// a render, or fired on a handler-free subtree) is a no-op, not an
    /// error. `payload` is the standard envelope the shim extracted; the
    /// listener's handler decides which field, if any, it consumes.
    pub fn dispatch(&mut self, path: &[u32], event: &str, payload: &EventPayload) -> Update {
        let Some(listener) = find_listener(&self.current, path, event) else {
            return Update::noop();
        };
        let msg = listener.handler.resolve(payload);
        self.step(msg)
    }

    /// Complete the pending command `id`. Unknown ids (a command raced a
    /// teardown, or the shim double-fired) are no-ops.
    pub fn resolve(&mut self, id: u32, payload: &FxPayload) -> Update {
        let Some(callback) = self.pending.remove(&id) else {
            return Update::noop();
        };
        let msg = match callback {
            CmdCallback::Simple(m) => m,
            CmdCallback::WithHttp(f) => f(HttpResult {
                ok: payload.ok.unwrap_or(false),
                status: payload.status.unwrap_or(0),
                body: payload.body.clone().unwrap_or_default(),
            }),
        };
        self.step(msg)
    }

    /// A subscription fired. Unknown ids (fired after its Stop was emitted
    /// but before the shim applied it) are no-ops.
    pub fn notify(&mut self, id: u32, payload: &FxPayload) -> Update {
        let Some(active) = self.active_subs.values().find(|s| s.id == id) else {
            return Update::noop();
        };
        let msg = match &active.callback {
            SubCallback::Simple(m) => m.clone(),
            SubCallback::WithNow(f) => f(payload.now.unwrap_or(0.0)),
        };
        self.step(msg)
    }

    pub fn model(&self) -> &Model {
        &self.model
    }

    /// One turn of the loop: update, re-render, diff, ledger bookkeeping.
    fn step(&mut self, msg: Msg) -> Update {
        let cmds = (self.update)(&mut self.model, msg);
        let new = (self.view)(&self.model);
        let patches = diff(&self.current, &new);
        let events = collect_events(&new);
        self.current = new;
        Update {
            patches,
            events,
            cmds: self.register_cmds(cmds),
            subs: self.diff_subs(),
        }
    }

    fn register_cmds(&mut self, cmds: Cmds<Msg>) -> Vec<CmdOut> {
        cmds.into_iter()
            .map(|Cmd { spec, callback }| {
                let id = self.fresh_id();
                self.pending.insert(id, callback);
                CmdOut { id, spec }
            })
            .collect()
    }

    /// Recompute `subscriptions(&model)` and diff against the active set by
    /// structural key: new keys start, vanished keys stop, retained keys
    /// keep their id but adopt the latest callback.
    fn diff_subs(&mut self) -> Vec<SubDelta> {
        let wanted = (self.subscriptions)(&self.model);
        let mut deltas = Vec::new();
        let mut next_active = BTreeMap::new();
        for Sub { spec, callback } in wanted {
            let key = spec.key();
            match self.active_subs.remove(&key) {
                Some(active) => {
                    next_active.insert(
                        key,
                        ActiveSub {
                            id: active.id,
                            callback,
                        },
                    );
                }
                None => {
                    let id = self.fresh_id();
                    deltas.push(SubDelta::Start { id, spec });
                    next_active.insert(key, ActiveSub { id, callback });
                }
            }
        }
        for (_, stale) in std::mem::take(&mut self.active_subs) {
            deltas.push(SubDelta::Stop { id: stale.id });
        }
        self.active_subs = next_active;
        deltas
    }

    fn fresh_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
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

    fn update(model: &mut i32, msg: Msg) -> Cmds<Msg> {
        match msg {
            Msg::Inc => *model += 1,
            Msg::Dec => *model -= 1,
        }
        Vec::new()
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
            vec![Patch::SetText {
                path: vec![1, 0],
                text: "1".into()
            }]
        );

        // Click "-" on the button element itself.
        let down = program.dispatch(&[0], "click", &none);
        assert_eq!(*program.model(), 0);
        assert_eq!(
            down.patches,
            vec![Patch::SetText {
                path: vec![1, 0],
                text: "0".into()
            }]
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

    fn form_update(model: &mut String, msg: FormMsg) -> Cmds<FormMsg> {
        match msg {
            FormMsg::Draft(s) => *model = s,
            FormMsg::Submit => model.push('!'),
        }
        Vec::new()
    }

    #[allow(clippy::ptr_arg)] // Model = String, and view must be fn(&Model)
    fn form_view(model: &String) -> VNode<FormMsg> {
        el("form")
            .on_submit(FormMsg::Submit)
            .child(
                el("input")
                    .attr("value", model.clone())
                    .on_input(FormMsg::Draft),
            )
            .into()
    }

    #[test]
    fn payload_value_flows_into_message() {
        let mut program = Program::new(String::new(), form_update, form_view);
        let typed = EventPayload {
            value: Some("hej".into()),
            ..Default::default()
        };
        let up = program.dispatch(&[0], "input", &typed);
        assert_eq!(program.model(), "hej");
        // The controlled input's value attr round-trips through the diff.
        assert_eq!(
            up.patches,
            vec![Patch::SetAttr {
                path: vec![0],
                name: "value".into(),
                value: "hej".into()
            }]
        );
    }

    #[test]
    fn submit_requests_prevent_default() {
        let mut program = Program::new(String::new(), form_update, form_view);
        let init = program.initial_render();
        let submit = init.events.iter().find(|s| s.name == "submit").unwrap();
        let input = init.events.iter().find(|s| s.name == "input").unwrap();
        assert!(submit.prevent_default);
        assert!(!input.prevent_default);
    }

    // --- effects ---------------------------------------------------------

    #[derive(Clone, PartialEq, Debug)]
    enum FxMsg {
        Kick,
        Done,
        Toggle,
        Tick(f64),
        Fetch,
        Got(HttpResult),
    }

    #[derive(Default)]
    struct FxModel {
        running: bool,
        last_now: f64,
        note: String,
    }

    fn fx_update(model: &mut FxModel, msg: FxMsg) -> Cmds<FxMsg> {
        match msg {
            FxMsg::Kick => return vec![delay(500, FxMsg::Done)],
            FxMsg::Done => model.note = "done".into(),
            FxMsg::Toggle => model.running = !model.running,
            FxMsg::Tick(now) => model.last_now = now,
            FxMsg::Fetch => return vec![http_get("./x.txt", FxMsg::Got)],
            FxMsg::Got(r) => {
                model.note = if r.ok {
                    r.body
                } else {
                    format!("err {}", r.status)
                }
            }
        }
        Vec::new()
    }

    fn fx_view(model: &FxModel) -> VNode<FxMsg> {
        el("div")
            .child(el("button").on("click", FxMsg::Kick).text("kick"))
            .child(el("button").on("click", FxMsg::Toggle).text("toggle"))
            .child(el("span").text(model.note.clone()))
            .into()
    }

    fn fx_subs(model: &FxModel) -> Vec<effects::Sub<FxMsg>> {
        if model.running {
            vec![every_with_now(100, FxMsg::Tick)]
        } else {
            Vec::new()
        }
    }

    fn fx_program() -> Program<FxModel, FxMsg> {
        Program::new(FxModel::default(), fx_update, fx_view).with_subscriptions(fx_subs)
    }

    #[test]
    fn command_roundtrip_delay() {
        let mut program = fx_program();
        program.initial_render();

        let kicked = program.dispatch(&[0], "click", &EventPayload::default());
        assert_eq!(kicked.cmds.len(), 1);
        assert_eq!(kicked.cmds[0].spec, effects::CmdSpec::Delay { ms: 500 });
        assert!(kicked.patches.is_empty());

        let done = program.resolve(kicked.cmds[0].id, &FxPayload::default());
        assert_eq!(program.model().note, "done");
        assert_eq!(
            done.patches,
            vec![Patch::SetText {
                path: vec![2, 0],
                text: "done".into()
            }]
        );

        // A command resolves exactly once.
        let again = program.resolve(kicked.cmds[0].id, &FxPayload::default());
        assert!(again.patches.is_empty() && again.cmds.is_empty());
    }

    #[test]
    fn http_result_flows_into_message() {
        let mut program = fx_program();
        program.initial_render();
        // Fetch via update directly (no button in the view for it).
        let fetched = program.step(FxMsg::Fetch);
        assert_eq!(
            fetched.cmds[0].spec,
            effects::CmdSpec::HttpGet {
                url: "./x.txt".into()
            }
        );

        let payload = FxPayload {
            ok: Some(true),
            status: Some(200),
            body: Some("hello".into()),
            now: None,
        };
        program.resolve(fetched.cmds[0].id, &payload);
        assert_eq!(program.model().note, "hello");
    }

    #[test]
    fn subscription_lifecycle_follows_model() {
        let mut program = fx_program();
        let init = program.initial_render();
        assert!(init.subs.is_empty());

        // Toggle on -> the interval starts.
        let on = program.dispatch(&[1], "click", &EventPayload::default());
        let SubDelta::Start { id, ref spec } = on.subs[0] else {
            panic!("expected Start, got {:?}", on.subs)
        };
        assert_eq!(*spec, effects::SubSpec::Every { ms: 100 });

        // Fire it twice; the payload clock reaches the model. No lifecycle
        // churn while the model still wants the sub.
        let tick = program.notify(
            id,
            &FxPayload {
                now: Some(123.0),
                ..Default::default()
            },
        );
        assert!(tick.subs.is_empty());
        assert_eq!(program.model().last_now, 123.0);
        program.notify(
            id,
            &FxPayload {
                now: Some(456.0),
                ..Default::default()
            },
        );
        assert_eq!(program.model().last_now, 456.0);

        // Toggle off -> the interval stops; late fires are no-ops.
        let off = program.dispatch(&[1], "click", &EventPayload::default());
        assert_eq!(off.subs, vec![SubDelta::Stop { id }]);
        let late = program.notify(
            id,
            &FxPayload {
                now: Some(789.0),
                ..Default::default()
            },
        );
        assert!(late.patches.is_empty());
        assert_eq!(program.model().last_now, 456.0);
    }

    #[test]
    fn init_cmds_are_emitted_once() {
        let mut program = fx_program().with_init(vec![http_get("./x.txt", FxMsg::Got)]);
        let init = program.initial_render();
        assert_eq!(init.cmds.len(), 1);
        program.resolve(
            init.cmds[0].id,
            &FxPayload {
                ok: Some(true),
                body: Some("boot".into()),
                ..Default::default()
            },
        );
        assert_eq!(program.model().note, "boot");
    }
}
