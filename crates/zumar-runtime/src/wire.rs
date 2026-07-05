//! The zumar binary wire format (version 1) — the outbound half of the
//! protocol. Inbound payloads stay as explicit scalar arguments (see the
//! `zumar_app!` macro); outbound render results dominate the boundary
//! traffic, so they get the compact encoding.
//!
//! Primitives:
//! - integers: unsigned LEB128 varints
//! - strings: varint byte length + UTF-8 bytes
//! - tags: single u8
//!
//! Message layouts (decoder: `www/zumar-wire.js`, kept in lockstep):
//!
//! ```text
//! InitialRender = ver:u8=1  node  events cmds subs
//! Update        = ver:u8=1  n:varint Patch*n  events cmds subs
//! node   = 0 str                     (text)
//!        | 1 str n:(str str)*n n:node*n   (element: tag, attrs, children)
//! path   = depth:varint idx:varint*depth
//! Patch  = 0 path node               (replace)
//!        | 1 path str                (setText)
//!        | 2 path str str            (setAttr)
//!        | 3 path str                (removeAttr)
//!        | 4 path n:node*n           (appendChildren)
//!        | 5 path len:varint         (truncateChildren)
//!        | 6 path idx:varint node    (insertChild)
//!        | 7 path from:varint to:varint  (moveChild)
//! events = n:(str pd:u8)*n
//! cmds   = n:(id:varint spec)*n ; spec = 0 ms:varint | 1 str
//! subs   = n:delta*n ; delta = 0 id:varint spec | 1 id:varint
//!        ; sub spec = 0 ms:varint    (every)
//! ```

use zumar_core::{EventSpec, Patch, SerNode};

use crate::effects::{CmdOut, CmdSpec, SubDelta, SubSpec};
use crate::{InitialRender, Update};

pub const WIRE_VERSION: u8 = 1;

impl InitialRender {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = vec![WIRE_VERSION];
        node(&mut buf, &self.root);
        tail(&mut buf, &self.events, &self.cmds, &self.subs);
        buf
    }
}

impl Update {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = vec![WIRE_VERSION];
        vu(&mut buf, self.patches.len() as u64);
        for p in &self.patches {
            patch(&mut buf, p);
        }
        tail(&mut buf, &self.events, &self.cmds, &self.subs);
        buf
    }
}

fn vu(buf: &mut Vec<u8>, mut n: u64) {
    loop {
        let byte = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            buf.push(byte);
            return;
        }
        buf.push(byte | 0x80);
    }
}

fn s(buf: &mut Vec<u8>, text: &str) {
    vu(buf, text.len() as u64);
    buf.extend_from_slice(text.as_bytes());
}

fn path(buf: &mut Vec<u8>, p: &[u32]) {
    vu(buf, p.len() as u64);
    for &i in p {
        vu(buf, i as u64);
    }
}

fn node(buf: &mut Vec<u8>, n: &SerNode) {
    match n {
        SerNode::Text { text } => {
            buf.push(0);
            s(buf, text);
        }
        SerNode::Element { tag, attrs, children } => {
            buf.push(1);
            s(buf, tag);
            vu(buf, attrs.len() as u64);
            for (name, value) in attrs {
                s(buf, name);
                s(buf, value);
            }
            vu(buf, children.len() as u64);
            for c in children {
                node(buf, c);
            }
        }
    }
}

fn patch(buf: &mut Vec<u8>, p: &Patch) {
    match p {
        Patch::Replace { path: pt, node: n } => {
            buf.push(0);
            path(buf, pt);
            node(buf, n);
        }
        Patch::SetText { path: pt, text } => {
            buf.push(1);
            path(buf, pt);
            s(buf, text);
        }
        Patch::SetAttr { path: pt, name, value } => {
            buf.push(2);
            path(buf, pt);
            s(buf, name);
            s(buf, value);
        }
        Patch::RemoveAttr { path: pt, name } => {
            buf.push(3);
            path(buf, pt);
            s(buf, name);
        }
        Patch::AppendChildren { path: pt, nodes } => {
            buf.push(4);
            path(buf, pt);
            vu(buf, nodes.len() as u64);
            for n in nodes {
                node(buf, n);
            }
        }
        Patch::TruncateChildren { path: pt, len } => {
            buf.push(5);
            path(buf, pt);
            vu(buf, *len as u64);
        }
        Patch::InsertChild { path: pt, index, node: n } => {
            buf.push(6);
            path(buf, pt);
            vu(buf, *index as u64);
            node(buf, n);
        }
        Patch::MoveChild { path: pt, from, to } => {
            buf.push(7);
            path(buf, pt);
            vu(buf, *from as u64);
            vu(buf, *to as u64);
        }
    }
}

fn tail(buf: &mut Vec<u8>, events: &[EventSpec], cmds: &[CmdOut], subs: &[SubDelta]) {
    vu(buf, events.len() as u64);
    for e in events {
        s(buf, &e.name);
        buf.push(e.prevent_default as u8);
    }
    vu(buf, cmds.len() as u64);
    for c in cmds {
        vu(buf, c.id as u64);
        match &c.spec {
            CmdSpec::Delay { ms } => {
                buf.push(0);
                vu(buf, *ms as u64);
            }
            CmdSpec::HttpGet { url } => {
                buf.push(1);
                s(buf, url);
            }
        }
    }
    vu(buf, subs.len() as u64);
    for d in subs {
        match d {
            SubDelta::Start { id, spec } => {
                buf.push(0);
                vu(buf, *id as u64);
                match spec {
                    SubSpec::Every { ms } => {
                        buf.push(0);
                        vu(buf, *ms as u64);
                    }
                }
            }
            SubDelta::Stop { id } => {
                buf.push(1);
                vu(buf, *id as u64);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_boundaries() {
        let mut buf = Vec::new();
        for n in [0u64, 1, 127, 128, 300, 16384, u32::MAX as u64] {
            buf.clear();
            vu(&mut buf, n);
            // decode back
            let mut val: u64 = 0;
            let mut shift = 0;
            for &b in &buf {
                val |= ((b & 0x7f) as u64) << shift;
                shift += 7;
            }
            assert_eq!(val, n);
        }
    }

    #[test]
    fn tiny_update_is_tiny() {
        let up = Update {
            patches: vec![Patch::SetText { path: vec![1, 0], text: "42".into() }],
            events: vec![EventSpec { name: "click".into(), prevent_default: false }],
            cmds: vec![],
            subs: vec![],
        };
        let wire = up.to_bytes();
        // ver + npatch + tag + path(3) + str(3) + events(1+7) + cmds(1) + subs(1)
        assert!(wire.len() < 25, "wire {} bytes", wire.len());
        let json = serde_json::to_string(&up).unwrap();
        assert!(wire.len() * 3 < json.len(), "wire {} vs json {}", wire.len(), json.len());
    }
}
