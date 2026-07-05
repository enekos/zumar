//! zuc — the zumar-lang compiler CLI.
//!
//!   zuc check <file.zu>                      parse + typecheck
//!   zuc build <file.zu> --out <dir> [--zumar <path>]
//!                                            emit a Rust crate speaking the
//!                                            zumar protocol (then build it
//!                                            with wasm-pack yourself)
//!
//! `--zumar` is the relative path from <dir> to the zumar repo root
//! (default: `../../..`).

use std::process::ExitCode;

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
    let usage = "usage: zuc check <file.zu> | zuc build <file.zu> --out <dir> [--zumar <path>]";
    let cmd = args.first().ok_or(usage)?;
    let file = args.get(1).ok_or(usage)?;
    let source = std::fs::read_to_string(file).map_err(|e| format!("{file}: {e}"))?;

    let app = zumar_lang::compile(&source).map_err(|e| format!("{file}:{e}"))?;

    match cmd.as_str() {
        "check" => Ok(format!(
            "{file}: ok — app {}, {} model field(s), {} message(s), all handled",
            app.name,
            app.model.len(),
            app.msgs.len()
        )),
        "build" => {
            let out = flag(args, "--out").ok_or(usage)?;
            let zumar = flag(args, "--zumar").unwrap_or_else(|| "../../..".to_string());
            let generated = zumar_lang::gen::generate(&app, &zumar);
            let src_dir = format!("{out}/src");
            std::fs::create_dir_all(&src_dir).map_err(|e| format!("{src_dir}: {e}"))?;
            std::fs::write(format!("{out}/Cargo.toml"), &generated.cargo_toml)
                .map_err(|e| e.to_string())?;
            std::fs::write(format!("{src_dir}/lib.rs"), &generated.lib_rs)
                .map_err(|e| e.to_string())?;
            Ok(format!(
                "{file}: compiled app {} -> {out}/ (crate `{}`, {} lines of Rust)\nnext: cd {out} && wasm-pack build --target web --out-dir ../www/pkg",
                app.name,
                generated.crate_name,
                generated.lib_rs.lines().count()
            ))
        }
        other => Err(format!("unknown command `{other}`\n{usage}")),
    }
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1).cloned())
}
