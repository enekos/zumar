//! zumar-core — virtual DOM, diff, and the patch protocol.
//!
//! Everything here is DOM-free and wasm-free: the tree is plain data, the
//! diff produces a serializable patch list, and event handlers are resolved
//! *inside* the tree (see [`find_handler`]) so patches never carry closures
//! or handler ids across the Wasm boundary. A thin JS shim applies patches
//! and reports events back as `(path, event-name)` pairs.

pub mod diff;
pub mod patch;
pub mod vdom;

pub use diff::diff;
pub use patch::{Patch, SerNode};
pub use vdom::{collect_events, el, find_handler, text, VElement, VNode};
