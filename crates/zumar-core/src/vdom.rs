use std::collections::BTreeMap;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// A virtual DOM node, parameterized over the app's message type.
#[derive(Debug, Clone, PartialEq)]
pub enum VNode<Msg> {
    Text(String),
    Element(VElement<Msg>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct VElement<Msg> {
    pub tag: String,
    /// Identity for keyed child diffing. Never rendered to the DOM.
    pub key: Option<String>,
    pub attrs: BTreeMap<String, String>,
    /// event name (e.g. "click") -> listener.
    pub events: BTreeMap<String, Listener<Msg>>,
    pub children: Vec<VNode<Msg>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Listener<Msg> {
    pub handler: Handler<Msg>,
    pub prevent_default: bool,
}

/// What a listener does with the event payload. Constructors are plain fn
/// pointers so `Msg` enum variants work directly: `.on_input(Msg::Draft)`.
///
/// `PartialEq` compares fn pointers by address — fine for its only use
/// (tests): the diff never compares events, since they don't cross the
/// boundary.
#[derive(Debug, Clone, PartialEq)]
#[allow(unpredictable_function_pointer_comparisons)]
pub enum Handler<Msg> {
    /// Fixed message; payload ignored.
    Simple(Msg),
    /// `event.target.value` (inputs, textareas, selects).
    WithValue(fn(String) -> Msg),
    /// `event.target.checked` (checkboxes, radios).
    WithChecked(fn(bool) -> Msg),
    /// `event.key` (keyboard events).
    WithKey(fn(String) -> Msg),
}

/// The standard envelope the JS shim extracts from every DOM event. Fields
/// the target doesn't have arrive as `None`. This is the pragmatic subset of
/// Elm's per-listener event decoders; a declared-fields protocol can replace
/// it later without touching handler code.
#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct EventPayload {
    pub value: Option<String>,
    pub checked: Option<bool>,
    pub key: Option<String>,
}

impl<Msg: Clone> Handler<Msg> {
    pub fn resolve(&self, payload: &EventPayload) -> Msg {
        match self {
            Handler::Simple(m) => m.clone(),
            Handler::WithValue(f) => f(payload.value.clone().unwrap_or_default()),
            Handler::WithChecked(f) => f(payload.checked.unwrap_or(false)),
            Handler::WithKey(f) => f(payload.key.clone().unwrap_or_default()),
        }
    }
}

/// One delegated listener the JS shim must install on the mount root.
/// `prevent_default` is per event *name* (OR over the whole tree) — coarse,
/// but submit/keydown are the realistic users and they want it globally.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
pub struct EventSpec {
    pub name: String,
    pub prevent_default: bool,
}

pub fn text<Msg>(s: impl Into<String>) -> VNode<Msg> {
    VNode::Text(s.into())
}

pub fn el<Msg>(tag: impl Into<String>) -> VElement<Msg> {
    VElement {
        tag: tag.into(),
        key: None,
        attrs: BTreeMap::new(),
        events: BTreeMap::new(),
        children: Vec::new(),
    }
}

impl<Msg> VElement<Msg> {
    pub fn key(mut self, key: impl Into<String>) -> Self {
        self.key = Some(key.into());
        self
    }

    pub fn attr(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.attrs.insert(name.into(), value.into());
        self
    }

    pub fn attr_if(self, cond: bool, name: impl Into<String>, value: impl Into<String>) -> Self {
        if cond {
            self.attr(name, value)
        } else {
            self
        }
    }

    pub fn listener(mut self, event: impl Into<String>, listener: Listener<Msg>) -> Self {
        self.events.insert(event.into(), listener);
        self
    }

    pub fn on(self, event: impl Into<String>, msg: Msg) -> Self {
        self.listener(
            event,
            Listener {
                handler: Handler::Simple(msg),
                prevent_default: false,
            },
        )
    }

    /// "submit" with preventDefault — the form never navigates.
    pub fn on_submit(self, msg: Msg) -> Self {
        self.listener(
            "submit",
            Listener {
                handler: Handler::Simple(msg),
                prevent_default: true,
            },
        )
    }

    pub fn on_input(self, f: fn(String) -> Msg) -> Self {
        self.listener(
            "input",
            Listener {
                handler: Handler::WithValue(f),
                prevent_default: false,
            },
        )
    }

    /// "change" carrying `event.target.checked` — checkboxes and radios.
    pub fn on_check(self, f: fn(bool) -> Msg) -> Self {
        self.listener(
            "change",
            Listener {
                handler: Handler::WithChecked(f),
                prevent_default: false,
            },
        )
    }

    pub fn on_keydown(self, f: fn(String) -> Msg) -> Self {
        self.listener(
            "keydown",
            Listener {
                handler: Handler::WithKey(f),
                prevent_default: false,
            },
        )
    }

    pub fn child(mut self, node: impl Into<VNode<Msg>>) -> Self {
        self.children.push(node.into());
        self
    }

    pub fn text(self, s: impl Into<String>) -> Self {
        self.child(VNode::Text(s.into()))
    }
}

impl<Msg> From<VElement<Msg>> for VNode<Msg> {
    fn from(e: VElement<Msg>) -> Self {
        VNode::Element(e)
    }
}

/// Resolve the listener for an event fired at `path`, honoring bubbling:
/// the deepest ancestor-or-self along `path` with a listener for `event`
/// wins.
///
/// `path` addresses a node by child indices from the root (`[]` = root
/// itself). Indices past the end of the tree are ignored — the DOM and the
/// vdom can briefly disagree if an event races a render, and dropping the
/// event is the correct outcome.
pub fn find_listener<'a, Msg>(
    root: &'a VNode<Msg>,
    path: &[u32],
    event: &str,
) -> Option<&'a Listener<Msg>> {
    let mut best = None;
    let mut node = root;
    if let VNode::Element(e) = node {
        if let Some(l) = e.events.get(event) {
            best = Some(l);
        }
    }
    for &i in path {
        let VNode::Element(e) = node else { break };
        match e.children.get(i as usize) {
            Some(child) => node = child,
            None => break,
        }
        if let VNode::Element(e) = node {
            if let Some(l) = e.events.get(event) {
                best = Some(l);
            }
        }
    }
    best
}

/// Every event name used anywhere in the tree, with its aggregated
/// preventDefault flag. The JS shim installs one delegated listener per
/// name on the mount root — nothing per-node.
pub fn collect_events<Msg>(root: &VNode<Msg>) -> Vec<EventSpec> {
    let mut map: BTreeMap<String, bool> = BTreeMap::new();
    walk(root, &mut map);
    return map
        .into_iter()
        .map(|(name, prevent_default)| EventSpec {
            name,
            prevent_default,
        })
        .collect();

    fn walk<Msg>(node: &VNode<Msg>, map: &mut BTreeMap<String, bool>) {
        if let VNode::Element(e) = node {
            for (name, listener) in &e.events {
                let pd = map.entry(name.clone()).or_insert(false);
                *pd = *pd || listener.prevent_default;
            }
            for c in &e.children {
                walk(c, map);
            }
        }
    }
}
