//! Differential harness for the diff: for arbitrary tree pairs (old, new),
//! applying `diff(old, new)` to a simulated DOM holding `old` must yield
//! exactly `new`. The applier mirrors www/zumar.js op-for-op (including
//! `moveChild`'s remove-then-insert semantics), so any divergence here is a
//! real browser-facing bug.
//!
//! Two generators: exhaustive permutations of keyed lists (every reorder of
//! 4 items against every other), and a seeded xorshift fuzz over random
//! trees with duplicate keys, mixed keyed/unkeyed children, and text nodes.

use zumar_core::{diff, el, text, Patch, SerNode, VNode};

// --- the simulated DOM (mirrors zumar.js `apply`) -----------------------

fn node_at<'a>(root: &'a mut SerNode, path: &[u32]) -> &'a mut SerNode {
    let mut n = root;
    for &i in path {
        let SerNode::Element { children, .. } = n else {
            panic!("patch path descends into a text node")
        };
        n = &mut children[i as usize];
    }
    n
}

fn apply(root: &mut SerNode, patches: Vec<Patch>) {
    for p in patches {
        match p {
            Patch::Replace { path, node } => *node_at(root, &path) = node,
            Patch::SetText { path, text } => {
                let SerNode::Text { text: t } = node_at(root, &path) else {
                    panic!("setText on a non-text node")
                };
                *t = text;
            }
            Patch::SetAttr { path, name, value } => {
                let SerNode::Element { attrs, .. } = node_at(root, &path) else {
                    panic!("setAttr on a text node")
                };
                attrs.insert(name, value);
            }
            Patch::RemoveAttr { path, name } => {
                let SerNode::Element { attrs, .. } = node_at(root, &path) else {
                    panic!("removeAttr on a text node")
                };
                attrs.remove(&name);
            }
            Patch::AppendChildren { path, nodes } => {
                let SerNode::Element { children, .. } = node_at(root, &path) else {
                    panic!("appendChildren on a text node")
                };
                children.extend(nodes);
            }
            Patch::TruncateChildren { path, len } => {
                let SerNode::Element { children, .. } = node_at(root, &path) else {
                    panic!("truncateChildren on a text node")
                };
                children.truncate(len as usize);
            }
            Patch::InsertChild { path, index, node } => {
                let SerNode::Element { children, .. } = node_at(root, &path) else {
                    panic!("insertChild on a text node")
                };
                children.insert(index as usize, node);
            }
            Patch::MoveChild { path, from, to } => {
                assert!(from > to, "moveChild must satisfy from > to (got {from} -> {to})");
                let SerNode::Element { children, .. } = node_at(root, &path) else {
                    panic!("moveChild on a text node")
                };
                let n = children.remove(from as usize);
                children.insert(to as usize, n);
            }
        }
    }
}

fn assert_converges(old: &VNode<()>, new: &VNode<()>, label: &str) {
    let patches = diff(old, new);
    let mut dom = SerNode::from_vnode(old);
    apply(&mut dom, patches);
    assert_eq!(
        dom,
        SerNode::from_vnode(new),
        "diff+apply diverged from target tree ({label})"
    );
}

// --- exhaustive keyed permutations --------------------------------------

fn keyed_list(perm: &[usize]) -> VNode<()> {
    let mut ul = el("ul");
    for &k in perm {
        ul = ul.child(el("li").key(format!("k{k}")).text(format!("item {k}")));
    }
    ul.into()
}

fn permutations(n: usize) -> Vec<Vec<usize>> {
    if n == 0 {
        return vec![vec![]];
    }
    let mut out = Vec::new();
    for p in permutations(n - 1) {
        for i in 0..=p.len() {
            let mut q = p.clone();
            q.insert(i, n - 1);
            out.push(q);
        }
    }
    out
}

#[test]
fn every_permutation_of_4_keyed_items_converges() {
    let perms = permutations(4);
    for a in &perms {
        for b in &perms {
            assert_converges(&keyed_list(a), &keyed_list(b), &format!("{a:?} -> {b:?}"));
        }
    }
}

#[test]
fn keyed_subsets_and_supersets_converge() {
    // Grow, shrink, and disjoint replacement, in both directions.
    let cases: &[&[usize]] = &[&[], &[0], &[0, 1, 2], &[2, 0], &[3, 4, 5], &[1, 3], &[5, 4, 3, 2, 1, 0]];
    for a in cases {
        for b in cases {
            assert_converges(&keyed_list(a), &keyed_list(b), &format!("{a:?} -> {b:?}"));
        }
    }
}

// --- seeded fuzz over arbitrary trees ------------------------------------

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64* — deterministic, no deps.
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

const TAGS: &[&str] = &["div", "span", "ul", "li", "p"];
const ATTRS: &[&str] = &["class", "id", "title", "data-x"];
const WORDS: &[&str] = &["aupa", "zumar", "elm", "", "hi there", "ñ木"];

fn gen_node(rng: &mut Rng, depth: u32) -> VNode<()> {
    if depth == 0 || rng.below(4) == 0 {
        return text(WORDS[rng.below(WORDS.len() as u64) as usize]);
    }
    let mut e = el(TAGS[rng.below(TAGS.len() as u64) as usize]);
    for _ in 0..rng.below(3) {
        e = e.attr(
            ATTRS[rng.below(ATTRS.len() as u64) as usize],
            WORDS[rng.below(WORDS.len() as u64) as usize],
        );
    }
    if rng.below(3) == 0 {
        // Small key space on purpose: collisions and duplicates are the
        // interesting cases.
        e = e.key(format!("k{}", rng.below(5)));
    }
    let children = rng.below(5);
    for _ in 0..children {
        e = e.child(gen_node(rng, depth - 1));
    }
    e.into()
}

#[test]
fn fuzz_random_tree_pairs_converge() {
    let mut rng = Rng(0x5eed_2026_0705);
    for i in 0..2_000 {
        let old = gen_node(&mut rng, 4);
        let new = gen_node(&mut rng, 4);
        assert_converges(&old, &new, &format!("fuzz iteration {i}"));
    }
}

#[test]
fn fuzz_identical_trees_produce_zero_patches() {
    let mut rng = Rng(0xda7a_2026_0705);
    for i in 0..500 {
        let tree = gen_node(&mut rng, 4);
        assert!(
            diff(&tree, &tree).is_empty(),
            "self-diff must be empty (iteration {i})"
        );
    }
}
