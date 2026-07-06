//! P0 spike: counter.zu live from the server (the LiveView analog).
//!
//! One `zumar_runtime::Program` per WebSocket connection; the model lives
//! here, the browser holds no state. The browser runs the stock zumar.js
//! shim plus the zumar-live.js transport adapter: DOM events become binary
//! frames up, wire-format patches stream back down over sutegi-ws.
//!
//! ```sh
//! cargo run                              # http://127.0.0.1:8080
//! PORT=8774 cargo run                    # demo port
//! node run.mjs                           # E2E harness (builds + spawns)
//! ```

mod counter;
mod frame;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use sutegi::prelude::*;
use zumar_core::EventPayload;
use zumar_runtime::effects::FxPayload;
use zumar_runtime::Program;

/// The per-connection program. The reactor already serializes callbacks per
/// connection; the inner mutex is only so the map stays a plain `Sync` static.
type Live = Arc<Mutex<Program<counter::Model, counter::Msg>>>;

static PROGRAMS: Mutex<Option<HashMap<u64, Live>>> = Mutex::new(None);

fn live(id: u64) -> Option<Live> {
    PROGRAMS
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|m| m.get(&id).cloned())
}

fn main() -> std::io::Result<()> {
    *PROGRAMS.lock().unwrap() = Some(HashMap::new());

    App::new("sutegi-zumar")
        .get("/", "The live counter page.", |_c| serve_file("index.html"))
        .get(
            "/www/:file",
            "Shim + adapter assets from zumar's www/.",
            |c| serve_file(c.param("file").unwrap_or("")),
        )
        .ws(
            "/live",
            "zumar live socket: a server-side Program per connection. First \
             frame down is an InitialRender, then Updates; frames up are \
             dispatch/resolve/notify (see src/frame.rs).",
            Ws::new()
                .on_open(|conn: &Conn, _req: &Request| {
                    let mut program = counter::program();
                    let init = program.initial_render().to_bytes();
                    if let Some(m) = PROGRAMS.lock().unwrap().as_mut() {
                        m.insert(conn.id(), Arc::new(Mutex::new(program)));
                    }
                    conn.send_binary(&init);
                })
                .on_message(|conn: &Conn, msg: Msg| {
                    let Msg::Binary(data) = msg else { return };
                    let Some(live) = live(conn.id()) else { return };
                    let update = match frame::decode(&data) {
                        Ok(frame::Frame::Dispatch {
                            path,
                            name,
                            value,
                            checked,
                            key,
                        }) => {
                            let payload = EventPayload {
                                value,
                                checked,
                                key,
                            };
                            live.lock().unwrap().dispatch(&path, &name, &payload)
                        }
                        Ok(frame::Frame::Resolve {
                            id,
                            ok,
                            status,
                            body,
                        }) => {
                            let payload = FxPayload {
                                ok: Some(ok),
                                status: Some(status),
                                body: Some(body),
                                now: None,
                            };
                            live.lock().unwrap().resolve(id, &payload)
                        }
                        Ok(frame::Frame::Notify { id, now }) => {
                            let payload = FxPayload {
                                now: Some(now),
                                ..FxPayload::default()
                            };
                            live.lock().unwrap().notify(id, &payload)
                        }
                        Err(e) => {
                            conn.close(1002, &e);
                            return;
                        }
                    };
                    conn.send_binary(&update.to_bytes());
                })
                .on_close(|conn: &Conn, _code| {
                    if let Some(m) = PROGRAMS.lock().unwrap().as_mut() {
                        m.remove(&conn.id());
                    }
                }),
        )
        .serve()
}

/// index.html is the spike's own; the shim files come straight from zumar's
/// www/ so the demo always runs the real, current framework JS.
fn serve_file(name: &str) -> Response {
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let (path, mime) = match name {
        "" | "index.html" => (base.join("www/index.html"), "text/html; charset=utf-8"),
        "zumar.js" | "zumar-wire.js" | "zumar-live.js" => {
            (base.join("../../www").join(name), "application/javascript")
        }
        _ => return text(404, "not found"),
    };
    match std::fs::read(&path) {
        Ok(bytes) => Response::new(200)
            .with_header("content-type", mime)
            .with_body(bytes),
        Err(_) => text(404, "not found"),
    }
}
