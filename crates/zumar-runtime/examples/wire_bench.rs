//! Wire-vs-JSON benchmark: `cargo run --release -p zumar-runtime --example wire_bench`
//!
//! Three representative messages: a big initial render (500-item keyed
//! list), a keyed-reorder update, and the hot-path tiny update (one
//! setText per event). Measures encoded size and encode time.

use std::collections::BTreeMap;
use std::time::Instant;

use zumar_core::{EventSpec, Patch, SerNode};
use zumar_runtime::{InitialRender, Update};

fn li(label: &str) -> SerNode {
    SerNode::Element {
        tag: "li".into(),
        attrs: BTreeMap::from([("class".into(), "open".into())]),
        children: vec![
            SerNode::Element {
                tag: "input".into(),
                attrs: BTreeMap::from([("type".into(), "checkbox".into())]),
                children: vec![],
            },
            SerNode::Element {
                tag: "span".into(),
                attrs: BTreeMap::new(),
                children: vec![SerNode::Text { text: label.into() }],
            },
        ],
    }
}

fn events() -> Vec<EventSpec> {
    vec![
        EventSpec { name: "change".into(), prevent_default: false },
        EventSpec { name: "click".into(), prevent_default: false },
        EventSpec { name: "input".into(), prevent_default: false },
        EventSpec { name: "submit".into(), prevent_default: true },
    ]
}

fn bench<T>(label: &str, value: &T, encode: impl Fn(&T) -> Vec<u8>)
where
    T: serde::Serialize,
{
    let json = serde_json::to_string(value).unwrap();
    let wire = encode(value);

    const N: u32 = 5_000;
    let t = Instant::now();
    for _ in 0..N {
        std::hint::black_box(serde_json::to_string(value).unwrap());
    }
    let json_ns = t.elapsed().as_nanos() / N as u128;
    let t = Instant::now();
    for _ in 0..N {
        std::hint::black_box(encode(value));
    }
    let wire_ns = t.elapsed().as_nanos() / N as u128;

    println!(
        "{label:<28} json {:>7} B / {:>7} ns    wire {:>6} B / {:>6} ns    ({:.1}x smaller, {:.1}x faster)",
        json.len(),
        json_ns,
        wire.len(),
        wire_ns,
        json.len() as f64 / wire.len() as f64,
        json_ns as f64 / wire_ns.max(1) as f64,
    );
}

fn main() {
    // 1. Initial render: 500-item list.
    let init = InitialRender {
        root: SerNode::Element {
            tag: "ul".into(),
            attrs: BTreeMap::from([("class".into(), "todos".into())]),
            children: (0..500).map(|i| li(&format!("todo item number {i}"))).collect(),
        },
        events: events(),
        cmds: vec![],
        subs: vec![],
    };
    bench("init: 500-item list", &init, |v| v.to_bytes());

    // 2. Keyed reorder: 40 moves + a handful of content patches.
    let reorder = Update {
        patches: (0..40)
            .map(|i| Patch::MoveChild { path: vec![3], from: 40 + i, to: i })
            .chain((0..8).map(|i| Patch::SetText {
                path: vec![3, i, 1, 0],
                text: format!("relabeled {i}"),
            }))
            .collect(),
        events: events(),
        cmds: vec![],
        subs: vec![],
    };
    bench("update: 40 moves + 8 texts", &reorder, |v| v.to_bytes());

    // 3. Hot path: one setText (every counter click, every keystroke).
    let tiny = Update {
        patches: vec![Patch::SetText { path: vec![2, 1, 0], text: "42".into() }],
        events: events(),
        cmds: vec![],
        subs: vec![],
    };
    bench("update: single setText", &tiny, |v| v.to_bytes());
}
