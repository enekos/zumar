//! Diff benchmark: `cargo run --release -p zumar-core --example diff_bench`
//!
//! Covers the shapes that matter in production: stable large lists (the
//! per-keystroke hot path), full reversal (worst case for the greedy keyed
//! matcher — O(n²) scan), single-item edits in large lists, and deep trees.

use std::time::Instant;

use zumar_core::{diff, el, text, VNode};

fn keyed_list(order: impl Iterator<Item = usize>) -> VNode<()> {
    let mut ul = el("ul");
    for k in order {
        ul = ul.child(
            el("li")
                .key(format!("k{k}"))
                .attr("class", "row")
                .child(el("span").text(format!("item number {k}"))),
        );
    }
    ul.into()
}

fn deep(depth: usize, label: &str) -> VNode<()> {
    let mut node: VNode<()> = text(label);
    for i in 0..depth {
        node = el("div")
            .attr("class", format!("level-{i}"))
            .child(node)
            .into();
    }
    node
}

fn bench(label: &str, old: &VNode<()>, new: &VNode<()>, iters: u32) {
    let patches = diff(old, new);
    let t = Instant::now();
    for _ in 0..iters {
        std::hint::black_box(diff(old, new));
    }
    let per = t.elapsed().as_micros() as f64 / iters as f64;
    println!("{label:<44} {per:>9.1} µs/diff   {} patches", patches.len());
}

fn main() {
    for n in [100, 1_000, 5_000] {
        let stable = keyed_list(0..n);
        bench(
            &format!("{n} keyed items, unchanged"),
            &stable,
            &stable.clone(),
            200,
        );

        let mut edited = keyed_list(0..n);
        if let VNode::Element(ul) = &mut edited {
            if let VNode::Element(li) = &mut ul.children[n / 2] {
                if let VNode::Element(span) = &mut li.children[0] {
                    span.children[0] = text("EDITED");
                }
            }
        }
        bench(&format!("{n} keyed items, 1 edited"), &stable, &edited, 200);

        let reversed = keyed_list((0..n).rev());
        bench(
            &format!("{n} keyed items, fully reversed"),
            &stable,
            &reversed,
            20,
        );
    }

    let a = deep(200, "bottom");
    let b = deep(200, "changed");
    bench("200-deep tree, leaf text changed", &a, &b, 500);
}
