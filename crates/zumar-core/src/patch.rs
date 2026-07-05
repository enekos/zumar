use std::collections::BTreeMap;

use serde::Serialize;

use crate::vdom::VNode;

/// A message-free, serializable snapshot of a subtree — what the JS shim
/// materializes with `document.createElement`. Events are deliberately
/// absent: dispatch is resolved vdom-side, so the DOM carries no handlers.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum SerNode {
    Text {
        text: String,
    },
    Element {
        tag: String,
        attrs: BTreeMap<String, String>,
        children: Vec<SerNode>,
    },
}

impl SerNode {
    pub fn from_vnode<Msg>(node: &VNode<Msg>) -> SerNode {
        match node {
            VNode::Text(t) => SerNode::Text { text: t.clone() },
            VNode::Element(e) => SerNode::Element {
                tag: e.tag.clone(),
                attrs: e.attrs.clone(),
                children: e.children.iter().map(SerNode::from_vnode).collect(),
            },
        }
    }
}

/// One DOM mutation. `path` addresses a node by child indices from the app
/// root (`[]` = the root itself). Patches are emitted in DFS order and are
/// safe to apply sequentially: a `Replace` ends recursion for its subtree,
/// and child insertions/removals only ever touch the tail of a child list.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum Patch {
    Replace {
        path: Vec<u32>,
        node: SerNode,
    },
    SetText {
        path: Vec<u32>,
        text: String,
    },
    SetAttr {
        path: Vec<u32>,
        name: String,
        value: String,
    },
    RemoveAttr {
        path: Vec<u32>,
        name: String,
    },
    AppendChildren {
        path: Vec<u32>,
        nodes: Vec<SerNode>,
    },
    TruncateChildren {
        path: Vec<u32>,
        len: u32,
    },
    /// Keyed diffing only. Insert `node` so it becomes child `index`.
    InsertChild {
        path: Vec<u32>,
        index: u32,
        node: SerNode,
    },
    /// Keyed diffing only. Move child `from` so it becomes child `to`.
    /// The diff guarantees `from > to`, which makes DOM `insertBefore`
    /// index-stable when applying.
    MoveChild {
        path: Vec<u32>,
        from: u32,
        to: u32,
    },
}
