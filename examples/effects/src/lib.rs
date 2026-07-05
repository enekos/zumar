//! Effects example — the M3 surface:
//!
//! - clock: `every_with_now` subscription, always active;
//! - stopwatch: an `every` subscription whose lifecycle follows the model
//!   (running → started, stopped → torn down), no manual bookkeeping;
//! - ping: chained `delay` commands (ping → pong → toast auto-clear);
//! - quote: `http_get` command, including one at init.
//!
//! Note there is still no clock, timer, or fetch on the Rust side — time
//! and IO only exist in the shim, and arrive here as messages.

use wasm_bindgen::prelude::*;

use zumar_core::{el, VNode};
use zumar_runtime::effects::{Cmds, HttpResult, Sub};
use zumar_runtime::{delay, every, every_with_now, http_get, Program};

#[derive(Clone)]
enum Msg {
    Tick(f64),
    SwTick,
    SwToggle,
    SwReset,
    Ping,
    Pong,
    ClearToast,
    Refetch,
    Got(HttpResult),
}

#[derive(Default)]
struct Model {
    now_ms: f64,
    running: bool,
    elapsed_ms: u32,
    toast: Option<String>,
    quote: Option<String>,
}

fn update(model: &mut Model, msg: Msg) -> Cmds<Msg> {
    match msg {
        Msg::Tick(now) => model.now_ms = now,
        Msg::SwTick => model.elapsed_ms += 100,
        Msg::SwToggle => model.running = !model.running,
        Msg::SwReset => model.elapsed_ms = 0,
        Msg::Ping => {
            model.toast = Some("ping…".into());
            return vec![delay(1500, Msg::Pong)];
        }
        Msg::Pong => {
            model.toast = Some("pong! (1.5s later)".into());
            return vec![delay(2000, Msg::ClearToast)];
        }
        Msg::ClearToast => model.toast = None,
        Msg::Refetch => {
            model.quote = None;
            return vec![http_get("./quote.txt", Msg::Got)];
        }
        Msg::Got(r) => {
            model.quote = Some(if r.ok {
                r.body.trim().to_string()
            } else {
                format!("fetch failed ({}): {}", r.status, r.body)
            });
        }
    }
    Vec::new()
}

fn subscriptions(model: &Model) -> Vec<Sub<Msg>> {
    let mut subs = vec![every_with_now(1000, Msg::Tick)];
    if model.running {
        subs.push(every(100, Msg::SwTick));
    }
    subs
}

fn clock(now_ms: f64) -> String {
    if now_ms == 0.0 {
        return "--:--:--".into();
    }
    let secs = (now_ms / 1000.0) as u64;
    format!(
        "{:02}:{:02}:{:02}",
        (secs / 3600) % 24,
        (secs / 60) % 60,
        secs % 60
    )
}

fn view(model: &Model) -> VNode<Msg> {
    el("div")
        .attr("class", "fx")
        .child(el("h1").text("zumar effects"))
        .child(
            el("section")
                .child(el("h2").text("utc clock (sub: every 1s, with now)"))
                .child(el("div").attr("class", "time").text(clock(model.now_ms))),
        )
        .child(
            el("section")
                .child(el("h2").text("stopwatch (sub lifecycle follows model)"))
                .child(el("div").attr("class", "time").text(format!(
                    "{}.{}s",
                    model.elapsed_ms / 1000,
                    (model.elapsed_ms % 1000) / 100
                )))
                .child(
                    el("div")
                        .attr("class", "row")
                        .child(
                            el("button")
                                .on("click", Msg::SwToggle)
                                .text(if model.running { "stop" } else { "start" }),
                        )
                        .child(el("button").on("click", Msg::SwReset).text("reset")),
                ),
        )
        .child(
            el("section")
                .child(el("h2").text("chained delays (cmd: delay)"))
                .child(
                    el("div")
                        .attr("class", "row")
                        .child(el("button").on("click", Msg::Ping).text("ping"))
                        .child(
                            el("span")
                                .attr("class", "toast")
                                .text(model.toast.clone().unwrap_or_default()),
                        ),
                ),
        )
        .child(
            el("section")
                .child(el("h2").text("http (cmd: httpGet, one at init)"))
                .child(
                    el("blockquote")
                        .text(model.quote.clone().unwrap_or_else(|| "fetching…".into())),
                )
                .child(el("button").on("click", Msg::Refetch).text("refetch")),
        )
        .into()
}

zumar_runtime::zumar_app!(
    App,
    Model,
    Msg,
    Program::new(Model::default(), update, view)
        .with_subscriptions(subscriptions)
        .with_init(vec![http_get("./quote.txt", Msg::Got)])
);
