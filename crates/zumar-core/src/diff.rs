use crate::patch::{Patch, SerNode};
use crate::vdom::VNode;

/// Diff two trees into a patch list.
///
/// Milestone-1 algorithm: unkeyed, positional. Same tag → diff attrs and
/// children pairwise, append/truncate the tail; anything else → replace the
/// subtree. Keyed diffing is a later milestone.
pub fn diff<Msg>(old: &VNode<Msg>, new: &VNode<Msg>) -> Vec<Patch> {
    let mut patches = Vec::new();
    diff_node(old, new, &mut Vec::new(), &mut patches);
    patches
}

fn diff_node<Msg>(old: &VNode<Msg>, new: &VNode<Msg>, path: &mut Vec<u32>, out: &mut Vec<Patch>) {
    match (old, new) {
        (VNode::Text(a), VNode::Text(b)) => {
            if a != b {
                out.push(Patch::SetText {
                    path: path.clone(),
                    text: b.clone(),
                });
            }
        }
        (VNode::Element(a), VNode::Element(b)) if a.tag == b.tag => {
            for (name, value) in &b.attrs {
                if a.attrs.get(name) != Some(value) {
                    out.push(Patch::SetAttr {
                        path: path.clone(),
                        name: name.clone(),
                        value: value.clone(),
                    });
                }
            }
            for name in a.attrs.keys() {
                if !b.attrs.contains_key(name) {
                    out.push(Patch::RemoveAttr {
                        path: path.clone(),
                        name: name.clone(),
                    });
                }
            }

            let shared = a.children.len().min(b.children.len());
            for i in 0..shared {
                path.push(i as u32);
                diff_node(&a.children[i], &b.children[i], path, out);
                path.pop();
            }
            if b.children.len() > shared {
                out.push(Patch::AppendChildren {
                    path: path.clone(),
                    nodes: b.children[shared..].iter().map(SerNode::from_vnode).collect(),
                });
            } else if a.children.len() > shared {
                out.push(Patch::TruncateChildren {
                    path: path.clone(),
                    len: shared as u32,
                });
            }
        }
        _ => {
            out.push(Patch::Replace {
                path: path.clone(),
                node: SerNode::from_vnode(new),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vdom::{el, text};

    type N = VNode<()>;

    #[test]
    fn identical_trees_produce_no_patches() {
        let a: N = el("div").child(text("hi")).into();
        let b: N = el("div").child(text("hi")).into();
        assert!(diff(&a, &b).is_empty());
    }

    #[test]
    fn text_change_is_set_text_at_path() {
        let a: N = el("div").child(el("span").text("1")).into();
        let b: N = el("div").child(el("span").text("2")).into();
        assert_eq!(
            diff(&a, &b),
            vec![Patch::SetText { path: vec![0, 0], text: "2".into() }]
        );
    }

    #[test]
    fn attr_add_change_remove() {
        let a: N = el("div").attr("class", "x").attr("id", "gone").into();
        let b: N = el("div").attr("class", "y").attr("title", "new").into();
        let patches = diff(&a, &b);
        assert!(patches.contains(&Patch::SetAttr { path: vec![], name: "class".into(), value: "y".into() }));
        assert!(patches.contains(&Patch::SetAttr { path: vec![], name: "title".into(), value: "new".into() }));
        assert!(patches.contains(&Patch::RemoveAttr { path: vec![], name: "id".into() }));
        assert_eq!(patches.len(), 3);
    }

    #[test]
    fn tag_change_replaces_subtree() {
        let a: N = el("div").child(el("span").text("x")).into();
        let b: N = el("div").child(el("p").text("x")).into();
        let patches = diff(&a, &b);
        assert_eq!(patches.len(), 1);
        assert!(matches!(&patches[0], Patch::Replace { path, .. } if path == &vec![0]));
    }

    #[test]
    fn child_list_grows_and_shrinks_at_tail() {
        let a: N = el("ul").child(el("li").text("a")).into();
        let b: N = el("ul").child(el("li").text("a")).child(el("li").text("b")).into();
        assert!(matches!(&diff(&a, &b)[0], Patch::AppendChildren { path, nodes } if path.is_empty() && nodes.len() == 1));
        assert_eq!(
            diff(&b, &a),
            vec![Patch::TruncateChildren { path: vec![], len: 1 }]
        );
    }
}
