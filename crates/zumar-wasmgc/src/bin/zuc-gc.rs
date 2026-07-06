//! zuc-gc — the WasmGC backend spike CLI.
//!
//!   zuc-gc <file.zu> -o <out.wasm>
//!
//! Emits a self-contained WasmGC module (no Rust toolchain, no wasm-bindgen)
//! for the supported subset; anything else errors with a pointer back to the
//! default Rust backend.

use std::process::ExitCode;

fn main() -> ExitCode {
    match run() {
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

fn run() -> Result<String, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let usage = "usage: zuc-gc <file.zu> -o <out.wasm>";
    let file = args.first().ok_or(usage)?;
    let out = args
        .iter()
        .position(|a| a == "-o")
        .and_then(|i| args.get(i + 1))
        .ok_or(usage)?;

    let source = std::fs::read_to_string(file).map_err(|e| format!("{file}: {e}"))?;
    let app = zumar_lang::compile(&source).map_err(|e| format!("{file}:{e}"))?;
    let bytes = zumar_wasmgc::emit(&app).map_err(|e| format!("{file}:{e}"))?;
    std::fs::write(out, &bytes).map_err(|e| format!("{out}: {e}"))?;
    Ok(format!(
        "{file}: emitted WasmGC module -> {out} ({} bytes, self-contained)",
        bytes.len()
    ))
}
