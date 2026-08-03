//! Command-line entry point:
//! `wasm2rs <input.wasm> [output] [funcs_per_file] [max_bytes_per_file]`.
//!
//! Reads a WebAssembly binary and writes the transpiled Rust source. With no
//! `funcs_per_file`/`max_bytes_per_file`, the whole module is one file, written
//! to `output` (or stdout when omitted). When either cap is positive, the module
//! is split into a `lib.rs` root plus `funcs_{n}.rs` chunk files written into the
//! `output` directory — the way to keep a very large module compilable while
//! bounding peak memory (`max_bytes_per_file` caps chunk size, so a few huge
//! functions cannot balloon one file).

use std::io::Write as _;
use std::process::ExitCode;

use wasm2rs::{SplitOptions, transpile_split};

// Transpiling a huge module churns roughly a gigabyte of output through
// countless short-lived `String` allocations. The system allocator keeps the
// freed pages resident (RSS climbs monotonically and never falls back after a
// chunk is emitted and dropped), so its retained/fragmented heap — not any one
// live structure — dominates peak memory. mimalloc returns memory to the OS far
// more readily: on the googlesql benchmark it cuts peak RSS from ~4.35GB to
// ~2.63GB. It is set only on the binary, so the library's own allocator choice
// is left to its embedder.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

const USAGE: &str = "usage: wasm2rs <input.wasm> [output] [funcs_per_file] [max_bytes_per_file]";

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
    let input = args.next().ok_or(USAGE)?;
    let output = args.next();
    let funcs_per_file = parse_usize_arg(args.next(), "funcs_per_file")?;
    let max_bytes_per_file = parse_usize_arg(args.next(), "max_bytes_per_file")?;

    let wasm = std::fs::read(&input).map_err(|e| format!("cannot read {input}: {e}"))?;

    // Splitting into files requires a target directory to write them into.
    if funcs_per_file > 0 || max_bytes_per_file > 0 {
        let dir = output.ok_or("splitting requires an output directory")?;
        return write_split(&wasm, &dir, funcs_per_file, max_bytes_per_file);
    }

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

/// Parse an optional numeric CLI argument, defaulting to `0` when absent.
fn parse_usize_arg(arg: Option<String>, name: &str) -> Result<usize, String> {
    match arg {
        Some(n) => n
            .parse::<usize>()
            .map_err(|_| format!("invalid {name} {n:?}; {USAGE}")),
        None => Ok(0),
    }
}

/// Transpile into `dir`, writing each generated file as it is produced so no
/// more than one chunk's worth is held in memory at a time.
fn write_split(
    wasm: &[u8],
    dir: &str,
    funcs_per_file: usize,
    max_bytes_per_file: usize,
) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {dir}: {e}"))?;
    let opts = SplitOptions {
        funcs_per_file,
        max_bytes_per_file,
    };
    let mut write_err: Option<String> = None;
    let result = transpile_split(wasm, &opts, |file| {
        let path = std::path::Path::new(dir).join(&file.name);
        if let Err(e) = std::fs::write(&path, &file.code) {
            // Record the first I/O failure and stop; the transpile itself is
            // still `Ok`, so this is surfaced separately below.
            write_err = Some(format!("cannot write {}: {e}", path.display()));
        }
        Ok(())
    });
    result.map_err(|e| e.to_string())?;
    match write_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}
