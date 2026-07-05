use std::collections::{BTreeMap, BTreeSet};

/// A virtual DOM node, parameterized over the app's message type.
#[derive(Debug, Clone, PartialEq)]
pub enum VNode<Msg> {
    Text(String),
    Element(VElement<Msg>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct VElement<Msg> {
    pub tag: String,
    pub attrs: BTreeMap<String, String>,
    /// event name (e.g. "click") -> message to dispatch.
    pub events: BTreeMap<String, Msg>,
    pub children: Vec<VNode<Msg>>,
}

pub fn text<Msg>(s: impl Into<String>) -> VNode<Msg> {
    VNode::Text(s.into())
}

pub fn el<Msg>(tag: impl Into<String>) -> VElement<Msg> {
    VElement {
        tag: tag.into(),
        attrs: BTreeMap::new(),
        events: BTreeMap::new(),
        children: Vec::new(),
    }
}

impl<Msg> VElement<Msg> {
    pub fn attr(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.attrs.insert(name.into(), value.into());
        self
    }

    pub fn on(mut self, event: impl Into<String>, msg: Msg) -> Self {
        self.events.insert(event.into(), msg);
        self
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

/// Resolve the message for an event fired at `path`, honoring bubbling:
/// the deepest ancestor-or-self along `path` with a handler for `event` wins.
///
/// `path` addresses a node by child indices from the root (`[]` = root
/// itself). Indices past the end of the tree are ignored — the DOM and the
/// vdom can briefly disagree if an event races a render, and dropping the
/// event is the correct outcome.
pub fn find_handler<'a, Msg>(root: &'a VNode<Msg>, path: &[u32], event: &str) -> Option<&'a Msg> {
    let mut best = None;
    let mut node = root;
    if let VNode::Element(e) = node {
        if let Some(m) = e.events.get(event) {
            best = Some(m);
        }
    }
    for &i in path {
        let VNode::Element(e) = node else { break };
        match e.children.get(i as usize) {
            Some(child) => node = child,
            None => break,
        }
        if let VNode::Element(e) = node {
            if let Some(m) = e.events.get(event) {
                best = Some(m);
            }
        }
    }
    best
}

/// Every event name used anywhere in the tree. The JS shim installs one
/// delegated listener per name on the mount root — nothing per-node.
pub fn collect_events<Msg>(root: &VNode<Msg>) -> Vec<String> {
    let mut set = BTreeSet::new();
    walk(root, &mut set);
    return set.into_iter().collect();

    fn walk<Msg>(node: &VNode<Msg>, set: &mut BTreeSet<String>) {
        if let VNode::Element(e) = node {
            set.extend(e.events.keys().cloned());
            for c in &e.children {
                walk(c, set);
            }
        }
    }
}
