//! Command-line entry point: `wasm2rs <input.wasm> [output.rs]`.
//!
//! Reads a WebAssembly binary and writes the transpiled Rust source to the
//! output file, or to stdout when no output path is given.

use std::io::Write as _;
use std::process::ExitCode;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("wasm2rs: {msg}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let input = args
        .next()
        .ok_or("usage: wasm2rs <input.wasm> [output.rs]")?;
    let output = args.next();

    let wasm = std::fs::read(&input).map_err(|e| format!("cannot read {input}: {e}"))?;
    let rust = wasm2rs::transpile(&wasm).map_err(|e| e.to_string())?;

    match output {
        Some(path) => {
            std::fs::write(&path, rust).map_err(|e| format!("cannot write {path}: {e}"))?;
        }
        None => {
            std::io::stdout()
                .write_all(rust.as_bytes())
                .map_err(|e| format!("cannot write to stdout: {e}"))?;
        }
    }
    Ok(())
}
