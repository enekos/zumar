use crate::patch::{Patch, SerNode};
use crate::vdom::VNode;

/// Diff two trees into a patch list.
///
/// Child lists where any element carries a `.key()` are diffed keyed
/// (match by key → move/insert, tail-truncate leftovers); unkeyed lists are
/// diffed positionally (pairwise + append/truncate the tail). Same tag →
/// diff attrs and children; anything else → replace the subtree.
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

            if has_keys(&a.children) || has_keys(&b.children) {
                diff_children_keyed(&a.children, &b.children, path, out);
            } else {
                diff_children_positional(&a.children, &b.children, path, out);
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

fn diff_children_positional<Msg>(
    old: &[VNode<Msg>],
    new: &[VNode<Msg>],
    path: &mut Vec<u32>,
    out: &mut Vec<Patch>,
) {
    let shared = old.len().min(new.len());
    for i in 0..shared {
        path.push(i as u32);
        diff_node(&old[i], &new[i], path, out);
        path.pop();
    }
    if new.len() > shared {
        out.push(Patch::AppendChildren {
            path: path.clone(),
            nodes: new[shared..].iter().map(SerNode::from_vnode).collect(),
        });
    } else if old.len() > shared {
        out.push(Patch::TruncateChildren {
            path: path.clone(),
            len: shared as u32,
        });
    }
}

fn has_keys<Msg>(children: &[VNode<Msg>]) -> bool {
    children
        .iter()
        .any(|c| matches!(c, VNode::Element(e) if e.key.is_some()))
}

/// Can `new` reuse the DOM node of `old`? Keyed elements match by key;
/// unkeyed elements by tag (positional fallback for stray unkeyed siblings
/// in a keyed list); text nodes always match each other.
fn reusable<Msg>(old: &VNode<Msg>, new: &VNode<Msg>) -> bool {
    match (old, new) {
        (VNode::Element(o), VNode::Element(n)) => match (&o.key, &n.key) {
            (Some(a), Some(b)) => a == b,
            (None, None) => o.tag == n.tag,
            _ => false,
        },
        (VNode::Text(_), VNode::Text(_)) => true,
        _ => false,
    }
}

/// Keyed child diff. Greedy: for each new position `i`, find a reusable
/// node at slot `j >= i` and move it forward (`from > to` always); no match
/// → insert. Unmatched leftovers are guaranteed to sit past `new.len()`
/// when the loop ends, so one TruncateChildren removes them all. Structural
/// ops are emitted before recursive diffs so child paths refer to settled
/// positions.
fn diff_children_keyed<Msg>(
    old: &[VNode<Msg>],
    new: &[VNode<Msg>],
    path: &mut Vec<u32>,
    out: &mut Vec<Patch>,
) {
    // Slots track the evolving child list: Some(i) = old child i, None =
    // freshly inserted (already complete, never re-diffed).
    let mut slots: Vec<Option<usize>> = (0..old.len()).map(Some).collect();

    for (i, new_child) in new.iter().enumerate() {
        let found = (i..slots.len())
            .find(|&j| slots[j].is_some_and(|oi| reusable(&old[oi], new_child)));
        match found {
            Some(j) if j == i => {}
            Some(j) => {
                out.push(Patch::MoveChild {
                    path: path.clone(),
                    from: j as u32,
                    to: i as u32,
                });
                let slot = slots.remove(j);
                slots.insert(i, slot);
            }
            None => {
                out.push(Patch::InsertChild {
                    path: path.clone(),
                    index: i as u32,
                    node: SerNode::from_vnode(new_child),
                });
                slots.insert(i, None);
            }
        }
    }

    if slots.len() > new.len() {
        out.push(Patch::TruncateChildren {
            path: path.clone(),
            len: new.len() as u32,
        });
    }

    for (i, new_child) in new.iter().enumerate() {
        if let Some(oi) = slots[i] {
            path.push(i as u32);
            diff_node(&old[oi], new_child, path, out);
            path.pop();
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

    fn item(key: &str, label: &str) -> N {
        el("li").key(key).text(label).into()
    }

    fn keyed_list(keys: &[&str]) -> N {
        let mut ul = el("ul");
        for k in keys {
            ul = ul.child(el("li").key(*k).text(*k));
        }
        ul.into()
    }

    #[test]
    fn keyed_reverse_is_moves_only() {
        let a = keyed_list(&["a", "b", "c"]);
        let b = keyed_list(&["c", "b", "a"]);
        let patches = diff(&a, &b);
        assert_eq!(
            patches,
            vec![
                Patch::MoveChild { path: vec![], from: 2, to: 0 },
                Patch::MoveChild { path: vec![], from: 2, to: 1 },
            ]
        );
    }

    #[test]
    fn keyed_insert_in_middle() {
        let a = keyed_list(&["a", "c"]);
        let b = keyed_list(&["a", "b", "c"]);
        let patches = diff(&a, &b);
        assert_eq!(patches.len(), 1);
        assert!(matches!(&patches[0], Patch::InsertChild { path, index: 1, .. } if path.is_empty()));
    }

    #[test]
    fn keyed_remove_in_middle_is_move_plus_truncate() {
        let a = keyed_list(&["a", "b", "c"]);
        let b = keyed_list(&["a", "c"]);
        assert_eq!(
            diff(&a, &b),
            vec![
                Patch::MoveChild { path: vec![], from: 2, to: 1 },
                Patch::TruncateChildren { path: vec![], len: 2 },
            ]
        );
    }

    #[test]
    fn keyed_match_still_diffs_content() {
        let a: N = el("ul").child(item("a", "old label")).into();
        let b: N = el("ul").child(item("a", "new label")).into();
        assert_eq!(
            diff(&a, &b),
            vec![Patch::SetText { path: vec![0, 0], text: "new label".into() }]
        );
    }

    #[test]
    fn keyed_moved_node_diffs_at_new_position() {
        let a: N = el("ul").child(item("a", "a")).child(item("b", "old")).into();
        let b: N = el("ul").child(item("b", "new")).child(item("a", "a")).into();
        assert_eq!(
            diff(&a, &b),
            vec![
                Patch::MoveChild { path: vec![], from: 1, to: 0 },
                Patch::SetText { path: vec![0, 0], text: "new".into() },
            ]
        );
    }

    #[test]
    fn unkeyed_siblings_in_keyed_list_are_reused_by_tag() {
        // A stray unkeyed separator between keyed items survives a reorder
        // of its neighbors without churn.
        let sep = || el("hr");
        let a: N = el("div").child(item("a", "a")).child(sep()).child(item("b", "b")).into();
        let b: N = el("div").child(item("b", "b")).child(sep()).child(item("a", "a")).into();
        let patches = diff(&a, &b);
        assert!(patches.iter().all(|p| matches!(p, Patch::MoveChild { .. })));
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
