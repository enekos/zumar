//! zuc — the zumar-lang compiler CLI.
//!
//!   zuc check <file.zu>                       parse + typecheck
//!   zuc build <file.zu> --out <dir> [--zumar <path>]
//!                                             emit the Rust crate
//!   zuc new <name> [--zumar <path>]           scaffold a project
//!   zuc dev [<file.zu>] [--port N] [--zumar <path>]
//!                                             build, serve, watch, reload
//!
//! The zumar repo root comes from `--zumar` or the `ZUMAR_HOME` env var.
//! Everything here is std-only: the dev server is a plain TcpListener, the
//! watcher polls mtimes, live reload is a polling script injected into
//! index.html as it is served.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use zumar_lang::ZuError;

// The JS half of the framework ships inside the compiler, so scaffolded
// projects are self-contained.
const SHIM_JS: &str = include_str!("../../../../www/zumar.js");
const WIRE_JS: &str = include_str!("../../../../www/zumar-wire.js");

const USAGE: &str = "usage:
  zuc check <file.zu>
  zuc build <file.zu> --out <dir> [--zumar <path>]
  zuc new <name> [--zumar <path>]
  zuc dev [<file.zu>] [--port N] [--zumar <path>]

the zumar repo root is taken from --zumar or $ZUMAR_HOME";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(msg) => {
            println!("{msg}");
            ExitCode::SUCCESS
        }
        Err(msg) => {
            eprintln!("{msg}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<String, String> {
    match args.first().map(String::as_str) {
        Some("check") => {
            let file = args.get(1).ok_or(USAGE)?;
            let (app, _) = compile_file(file)?;
            Ok(format!(
                "{file}: ok — app {}, {} model field(s), {} message(s), all handled",
                app.name,
                app.model.len(),
                app.msgs.len()
            ))
        }
        Some("build") => {
            let file = args.get(1).ok_or(USAGE)?;
            let out = flag(args, "--out").ok_or(USAGE)?;
            let zumar = zumar_path(args)?;
            let (app, _) = compile_file(file)?;
            let generated = write_crate(&app, Path::new(&out), &zumar)?;
            Ok(format!(
                "{file}: compiled app {} -> {out}/ (crate `{}`)\nnext: zuc dev, or wasm-pack build {out} --target web",
                app.name, generated
            ))
        }
        Some("new") => scaffold(args.get(1).ok_or(USAGE)?),
        Some("dev") => dev(args),
        _ => Err(USAGE.into()),
    }
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn zumar_path(args: &[String]) -> Result<PathBuf, String> {
    let raw = flag(args, "--zumar")
        .or_else(|| std::env::var("ZUMAR_HOME").ok())
        .ok_or("can't locate the zumar repo: pass --zumar <path> or set ZUMAR_HOME")?;
    std::fs::canonicalize(&raw).map_err(|e| format!("--zumar path `{raw}`: {e}"))
}

// --- compilation with caret diagnostics ----------------------------------

fn compile_file(file: &str) -> Result<(zumar_lang::App, String), String> {
    let source = std::fs::read_to_string(file).map_err(|e| format!("{file}: {e}"))?;
    match zumar_lang::compile(&source) {
        Ok(app) => Ok((app, source)),
        Err(e) => Err(report(file, &source, &e)),
    }
}

/// Elm-style diagnostic: position, message, offending line, caret.
fn report(file: &str, src: &str, e: &ZuError) -> String {
    let mut out = format!("{file}:{}:{}: error: {}\n", e.line, e.col, e.msg);
    if let Some(line) = src.lines().nth(e.line.saturating_sub(1)) {
        let gutter = format!("{:>4} | ", e.line);
        out.push_str(&format!("{gutter}{line}\n"));
        out.push_str(&format!(
            "{}^\n",
            " ".repeat(gutter.len() + e.col.saturating_sub(1))
        ));
    }
    out.trim_end().to_string()
}

fn write_crate(app: &zumar_lang::App, out: &Path, zumar: &Path) -> Result<String, String> {
    let generated = zumar_lang::gen::generate(app, &zumar.display().to_string());
    let src_dir = out.join("src");
    std::fs::create_dir_all(&src_dir).map_err(|e| format!("{}: {e}", src_dir.display()))?;
    std::fs::write(out.join("Cargo.toml"), &generated.cargo_toml).map_err(|e| e.to_string())?;
    std::fs::write(src_dir.join("lib.rs"), &generated.lib_rs).map_err(|e| e.to_string())?;
    Ok(generated.crate_name)
}

// --- zuc new --------------------------------------------------------------

fn scaffold(name: &str) -> Result<String, String> {
    let valid = name.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && name.chars().all(|c| c.is_ascii_alphanumeric());
    if !valid {
        return Err(format!(
            "project name `{name}` must be alphanumeric and start with a letter"
        ));
    }
    let root = PathBuf::from(name);
    if root.exists() {
        return Err(format!("`{name}` already exists"));
    }
    let app_name = {
        let mut cs = name.chars();
        cs.next()
            .map(|c| c.to_ascii_uppercase())
            .into_iter()
            .collect::<String>()
            + cs.as_str()
    };
    let crate_name = name.to_lowercase();

    let zu = format!(
        r#"# {name}.zu — edit me, `zuc dev` rebuilds on save.

app {app_name}

model {{ count: Int }}

init = {{ count = 0 }}

msg Inc | Dec | Reset

update Inc = {{ count = model.count + 1 }}
update Dec = {{ count = model.count - 1 }}
update Reset = {{ count = 0 }}

view =
  div [class "app"] [
    h1 [] [ text "{app_name}" ],
    div [class "row"] [
      button [onClick Dec] [ text "-" ],
      span [class "count"] [ text show(model.count) ],
      button [onClick Inc] [ text "+" ]
    ],
    button [class "reset", onClick Reset] [ text "reset" ]
  ]
"#
    );

    let index = format!(
        r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>{app_name}</title>
  <style>
    :root {{ color-scheme: dark; }}
    body {{ margin: 0; min-height: 100vh; display: grid; place-items: center;
           background: #12140f; color: #e8e6d9;
           font-family: ui-monospace, "SF Mono", Menlo, monospace; }}
    .app {{ text-align: center; }}
    .app h1 {{ letter-spacing: 0.2em; color: #9db668; }}
    .row {{ display: flex; align-items: center; justify-content: center; gap: 1.2rem; margin: 2rem 0 1rem; }}
    .count {{ font-size: 3rem; min-width: 4ch; font-variant-numeric: tabular-nums; }}
    button {{ background: #1e2318; color: #e8e6d9; border: 1px solid #3a4230;
             border-radius: 8px; font: inherit; font-size: 1.4rem;
             width: 3rem; height: 3rem; cursor: pointer; }}
    button:hover {{ border-color: #9db668; }}
    button.reset {{ width: auto; height: auto; font-size: 0.8rem; padding: 0.4rem 1rem; color: #6f7362; }}
  </style>
</head>
<body>
  <div id="app"></div>
  <script type="module">
    import init, {{ App }} from "./pkg/{crate_name}.js";
    import {{ mount }} from "./zumar.js";

    await init();
    mount(new App(), document.getElementById("app"));
  </script>
</body>
</html>
"#
    );

    let www = root.join("www");
    std::fs::create_dir_all(&www).map_err(|e| e.to_string())?;
    std::fs::write(root.join(format!("{name}.zu")), zu).map_err(|e| e.to_string())?;
    std::fs::write(www.join("index.html"), index).map_err(|e| e.to_string())?;
    std::fs::write(www.join("zumar.js"), SHIM_JS).map_err(|e| e.to_string())?;
    std::fs::write(www.join("zumar-wire.js"), WIRE_JS).map_err(|e| e.to_string())?;
    std::fs::write(root.join(".gitignore"), "app/\nwww/pkg/\n").map_err(|e| e.to_string())?;

    Ok(format!(
        "created {name}/\n  {name}/{name}.zu       the program\n  {name}/www/           static assets + framework shim\nnext: cd {name} && zuc dev"
    ))
}

// --- zuc dev ---------------------------------------------------------------

fn dev(args: &[String]) -> Result<String, String> {
    let file = match args.get(1).filter(|a| !a.starts_with("--")) {
        Some(f) => f.clone(),
        None => find_single_zu()?,
    };
    let port: u16 = match flag(args, "--port") {
        Some(p) => p
            .parse()
            .map_err(|_| format!("--port `{p}` is not a number"))?,
        None => 8900,
    };
    let zumar = zumar_path(args)?;
    let proj = std::fs::canonicalize(&file)
        .map_err(|e| format!("{file}: {e}"))?
        .parent()
        .map(Path::to_path_buf)
        .ok_or("can't resolve project directory")?;
    let www = proj.join("www");
    if !www.join("index.html").exists() {
        return Err(format!(
            "{} has no www/index.html — `zuc new <name>` scaffolds the expected layout",
            proj.display()
        ));
    }

    // First build must succeed so there is something to serve.
    build_wasm(&file, &proj, &zumar)?;

    let counter = Arc::new(AtomicU64::new(1));
    {
        let www = www.clone();
        let counter = counter.clone();
        std::thread::spawn(move || serve(www, counter, port));
    }
    println!("zuc dev: serving http://127.0.0.1:{port}  (watching {file}, ctrl-c to stop)");

    // Watch loop: rebuild on .zu save, reload on index.html save.
    let index = www.join("index.html");
    let mut zu_stamp = mtime(Path::new(&file));
    let mut html_stamp = mtime(&index);
    loop {
        std::thread::sleep(Duration::from_millis(300));
        let z = mtime(Path::new(&file));
        if z != zu_stamp {
            zu_stamp = z;
            match build_wasm(&file, &proj, &zumar) {
                Ok(elapsed) => {
                    counter.fetch_add(1, Ordering::SeqCst);
                    println!("zuc dev: rebuilt in {elapsed:.1}s — reloading");
                }
                Err(e) => eprintln!("{e}\nzuc dev: still serving the last good build"),
            }
        }
        let h = mtime(&index);
        if h != html_stamp {
            html_stamp = h;
            counter.fetch_add(1, Ordering::SeqCst);
            println!("zuc dev: index.html changed — reloading");
        }
    }
}

fn find_single_zu() -> Result<String, String> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(".").map_err(|e| e.to_string())?.flatten() {
        let p = entry.path();
        if p.extension().is_some_and(|e| e == "zu") {
            found.push(p.display().to_string());
        }
    }
    match found.as_slice() {
        [one] => Ok(one.clone()),
        [] => Err("no .zu file in the current directory (or pass one: zuc dev app.zu)".into()),
        many => Err(format!(
            "multiple .zu files ({}) — pass one explicitly",
            many.join(", ")
        )),
    }
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

/// Compile the .zu, regenerate the crate, run wasm-pack (dev profile).
/// Returns elapsed seconds.
fn build_wasm(file: &str, proj: &Path, zumar: &Path) -> Result<f64, String> {
    let started = std::time::Instant::now();
    let (app, _) = compile_file(file)?;
    let app_dir = proj.join("app");
    write_crate(&app, &app_dir, zumar)?;

    let output = Command::new("wasm-pack")
        .args(["build", "--dev", "--target", "web", "--out-dir"])
        .arg(proj.join("www/pkg"))
        .arg(&app_dir)
        .output()
        .map_err(|e| format!("running wasm-pack: {e} (install: cargo install wasm-pack)"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail: Vec<&str> = stderr.lines().rev().take(25).collect();
        return Err(format!(
            "wasm-pack failed:\n{}",
            tail.into_iter().rev().collect::<Vec<_>>().join("\n")
        ));
    }
    Ok(started.elapsed().as_secs_f64())
}

// --- the dev server ---------------------------------------------------------

const RELOAD_SNIPPET: &str = r#"<script>/* zuc dev live reload */
(async () => {
  let last = null;
  for (;;) {
    try {
      const b = await (await fetch("/__build", { cache: "no-store" })).text();
      if (last !== null && b !== last) location.reload();
      last = b;
    } catch {}
    await new Promise((r) => setTimeout(r, 400));
  }
})();
</script>"#;

fn serve(www: PathBuf, counter: Arc<AtomicU64>, port: u16) {
    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("zuc dev: can't bind port {port}: {e}");
            std::process::exit(1);
        }
    };
    for stream in listener.incoming().flatten() {
        let www = www.clone();
        let counter = counter.clone();
        std::thread::spawn(move || {
            let _ = handle(stream, &www, &counter);
        });
    }
}

fn handle(mut stream: TcpStream, www: &Path, counter: &AtomicU64) -> std::io::Result<()> {
    let mut buf = [0u8; 4096];
    let n = stream.read(&mut buf)?;
    let request = String::from_utf8_lossy(&buf[..n]);
    let path = request
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or("/");

    if path == "/__build" {
        let body = counter.load(Ordering::SeqCst).to_string();
        return respond(&mut stream, 200, "text/plain", body.into_bytes());
    }

    let rel = path.trim_start_matches('/').split('?').next().unwrap_or("");
    let rel = if rel.is_empty() { "index.html" } else { rel };
    // Path traversal guard: serve strictly from within www/.
    if rel
        .split('/')
        .any(|seg| seg == ".." || seg.is_empty() && rel.contains("//"))
    {
        return respond(&mut stream, 403, "text/plain", b"forbidden".to_vec());
    }
    let full = www.join(rel);
    match std::fs::canonicalize(&full) {
        Ok(canon) if canon.starts_with(std::fs::canonicalize(www)?) => {
            let mut body = std::fs::read(&canon)?;
            if canon.extension().is_some_and(|e| e == "html") {
                let html = String::from_utf8_lossy(&body);
                body = match html.rfind("</body>") {
                    Some(i) => {
                        format!("{}{}{}", &html[..i], RELOAD_SNIPPET, &html[i..]).into_bytes()
                    }
                    None => format!("{html}{RELOAD_SNIPPET}").into_bytes(),
                };
            }
            respond(&mut stream, 200, mime(&canon), body)
        }
        _ => respond(&mut stream, 404, "text/plain", b"not found".to_vec()),
    }
}

fn mime(p: &Path) -> &'static str {
    match p.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "wasm" => "application/wasm",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn respond(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: Vec<u8>,
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        403 => "Forbidden",
        _ => "Not Found",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(&body)
}
