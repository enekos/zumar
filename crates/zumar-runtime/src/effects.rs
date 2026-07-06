//! Commands (one-shot effects) and subscriptions (long-lived effects).
//!
//! Both are split the same way the vdom's events are: a serializable *spec*
//! crosses the boundary for the JS shim to execute, while the Msg-producing
//! callback stays wasm-side, keyed by a runtime-assigned id. The shim
//! re-enters the program with `resolve(id, payload)` when a command
//! completes and `notify(id, payload)` each time a subscription fires.

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// A one-shot effect requested by `update`. Executed once by the shim;
/// its completion re-enters the program via `resolve`.
pub struct Cmd<Msg> {
    pub(crate) spec: CmdSpec,
    pub(crate) callback: CmdCallback<Msg>,
}

/// What `update` returns. `Vec::new()` = no effects (Elm's `Cmd.none`).
pub type Cmds<Msg> = Vec<Cmd<Msg>>;

/// The shim-executable half of a command.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize))]
#[cfg_attr(feature = "serde", serde(tag = "kind", rename_all = "camelCase"))]
pub enum CmdSpec {
    Delay {
        ms: u32,
    },
    HttpGet {
        url: String,
    },
    /// Publish `message` to a pubsub `topic`. Fire-and-forget: no completion
    /// re-enters the program. Only a live-mode host (sutegi-zumar) can act
    /// on it; in client mode the shim warns and drops it (no server bus).
    Publish {
        topic: String,
        message: String,
    },
}

#[allow(unpredictable_function_pointer_comparisons)] // test-only equality, like Handler
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CmdCallback<Msg> {
    Simple(Msg),
    WithHttp(fn(HttpResult) -> Msg),
    /// Fire-and-forget (publish): never resolves, so it is not tracked in the
    /// pending table — no follow-up Msg, no leak.
    Fire,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HttpResult {
    pub ok: bool,
    pub status: u16,
    pub body: String,
}

/// Envelope for `resolve`/`notify` payloads — the effects-side analog of
/// `EventPayload`. Fields irrelevant to the completing effect are `None`.
#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct FxPayload {
    pub ok: Option<bool>,
    pub status: Option<u16>,
    pub body: Option<String>,
    /// Milliseconds since the Unix epoch, from the shim's clock — the only
    /// time source; wasm-side code never reads a clock.
    pub now: Option<f64>,
}

/// Fire `msg` once after `ms` milliseconds.
pub fn delay<Msg>(ms: u32, msg: Msg) -> Cmd<Msg> {
    Cmd {
        spec: CmdSpec::Delay { ms },
        callback: CmdCallback::Simple(msg),
    }
}

/// GET `url`; the response (or network error, as `ok: false, status: 0`)
/// arrives through `f`.
pub fn http_get<Msg>(url: impl Into<String>, f: fn(HttpResult) -> Msg) -> Cmd<Msg> {
    Cmd {
        spec: CmdSpec::HttpGet { url: url.into() },
        callback: CmdCallback::WithHttp(f),
    }
}

/// Publish `message` to `topic`. Fire-and-forget — the pair of [`topic`]:
/// one connection publishes, every connection subscribed to that topic gets
/// a message. Live-mode only (needs a server-side bus).
pub fn publish<Msg>(topic: impl Into<String>, message: impl Into<String>) -> Cmd<Msg> {
    Cmd {
        spec: CmdSpec::Publish {
            topic: topic.into(),
            message: message.into(),
        },
        callback: CmdCallback::Fire,
    }
}

/// A long-lived effect derived from model state. `subscriptions(&model)` is
/// recomputed after every update and diffed against the active set — subs
/// that appear start, subs that disappear stop, exactly like event specs.
pub struct Sub<Msg> {
    pub(crate) spec: SubSpec,
    pub(crate) callback: SubCallback<Msg>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize))]
#[cfg_attr(feature = "serde", serde(tag = "kind", rename_all = "camelCase"))]
pub enum SubSpec {
    Every {
        ms: u32,
    },
    /// Subscribe to a pubsub `topic`; each published message fires the sub
    /// with that message as the payload. Live-mode only (needs a server bus)
    /// — the pair of [`publish`].
    Topic {
        name: String,
    },
}

impl SubSpec {
    /// Structural identity for lifecycle diffing. Two subs with the same
    /// spec are the same subscription (last callback wins) — mirror of
    /// Elm's batch dedup.
    pub(crate) fn key(&self) -> String {
        match self {
            SubSpec::Every { ms } => format!("every:{ms}"),
            SubSpec::Topic { name } => format!("topic:{name}"),
        }
    }
}

#[allow(unpredictable_function_pointer_comparisons)]
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SubCallback<Msg> {
    Simple(Msg),
    WithNow(fn(f64) -> Msg),
    /// Deliver the fired payload string (a published topic message) to `f`.
    WithBody(fn(String) -> Msg),
}

/// Fire `msg` every `ms` milliseconds while the sub is active.
pub fn every<Msg>(ms: u32, msg: Msg) -> Sub<Msg> {
    Sub {
        spec: SubSpec::Every { ms },
        callback: SubCallback::Simple(msg),
    }
}

/// Like [`every`], but the message carries the shim's epoch-ms clock.
pub fn every_with_now<Msg>(ms: u32, f: fn(f64) -> Msg) -> Sub<Msg> {
    Sub {
        spec: SubSpec::Every { ms },
        callback: SubCallback::WithNow(f),
    }
}

/// Subscribe to `topic`; each message published there arrives through `f`.
/// The pair of [`publish`]. Live-mode only.
pub fn topic<Msg>(name: impl Into<String>, f: fn(String) -> Msg) -> Sub<Msg> {
    Sub {
        spec: SubSpec::Topic { name: name.into() },
        callback: SubCallback::WithBody(f),
    }
}

/// A command instance handed to the shim: execute `spec`, call
/// `resolve(id, ...)` when done.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize))]
pub struct CmdOut {
    pub id: u32,
    pub spec: CmdSpec,
}

/// A subscription lifecycle change for the shim to apply.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize))]
#[cfg_attr(feature = "serde", serde(tag = "op", rename_all = "camelCase"))]
pub enum SubDelta {
    Start { id: u32, spec: SubSpec },
    Stop { id: u32 },
}
