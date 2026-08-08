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

use wasm2rs::{
    SplitOptions, TranspileOptions, cargo_manifest, transpile_split_with_options,
    transpile_with_options,
};

// Transpiling a huge module churns roughly a gigabyte of output through
// countless short-lived `String` allocations. The system allocator keeps the
// freed pages resident (RSS climbs monotonically and never falls back after a
// chunk is emitted and dropped), so its retained/fragmented heap — not any one
// live structure — dominates peak memory. mimalloc returns memory to the OS far
// more readily: on the googlesql benchmark it cuts peak RSS from ~4.35GB to
// ~2.63GB. It is set only on the binary, so the library's own allocator choice
// is left to its embedder. Gated on the `cli` feature (on by default) so a
// library consumer building with `default-features = false` never compiles
// mimalloc's C code; without it the binary just falls back to the system
// allocator.
#[cfg(feature = "cli")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

const USAGE: &str =
    "usage: wasm2rs [--unsafe-memory] <input.wasm> [output] [funcs_per_file] [max_bytes_per_file]";

/// Worker-thread stack size. Flattening a deeply nested function recurses on the
/// module's control-flow nesting, which a pathological module can drive into the
/// thousands — past the default main-thread stack. Like `rustc`, run the work on
/// a thread with a generous stack.
const WORKER_STACK: usize = 512 * 1024 * 1024;

fn main() -> ExitCode {
    let worker = std::thread::Builder::new()
        .stack_size(WORKER_STACK)
        .spawn(run)
        .expect("spawn worker thread");
    let result = match worker.join() {
        Ok(result) => result,
        Err(_) => Err("transpiler thread panicked".to_string()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("wasm2rs: {msg}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    // Flags may appear anywhere; the remaining positional arguments keep their
    // order. `--unsafe-memory` opts into unchecked linear-memory access.
    let mut unsafe_memory = false;
    let mut positional: Vec<String> = Vec::new();
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--unsafe-memory" => unsafe_memory = true,
            _ => positional.push(arg),
        }
    }
    let mut args = positional.into_iter();
    let input = args.next().ok_or(USAGE)?;
    let output = args.next();
    let funcs_per_file = parse_usize_arg(args.next(), "funcs_per_file")?;
    let max_bytes_per_file = parse_usize_arg(args.next(), "max_bytes_per_file")?;

    let wasm = std::fs::read(&input).map_err(|e| format!("cannot read {input}: {e}"))?;
    let topts = TranspileOptions { unsafe_memory };

    // Splitting into files requires a target directory to write them into.
    if funcs_per_file > 0 || max_bytes_per_file > 0 {
        let dir = output.ok_or("splitting requires an output directory")?;
        return write_split(&wasm, &dir, funcs_per_file, max_bytes_per_file, &topts);
    }

    let rust = transpile_with_options(&wasm, &topts).map_err(|e| e.to_string())?;
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

/// Derive a valid Cargo package name from the output directory's final
/// component. Non-alphanumeric characters become `_` (so the default lib target,
/// which mirrors the package name, is a legal Rust identifier); a name that is
/// empty or starts with a digit is prefixed to stay a valid crate name.
fn crate_name(dir: &str) -> String {
    let base = std::path::Path::new(dir)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    let sanitized: String = base
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    match sanitized.chars().next() {
        None => "wasm_module".to_string(),
        Some(c) if c.is_ascii_digit() => format!("m_{sanitized}"),
        Some(_) => sanitized,
    }
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
    topts: &TranspileOptions,
) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {dir}: {e}"))?;

    // The chunks form a crate but carry no build settings of their own. Drop a
    // recommended `Cargo.toml` (size-optimized release profile, `[lib]` pointed
    // at `lib.rs`) next to them so the emitted crate builds compactly out of the
    // box. The package name is derived from the output directory.
    let manifest_path = std::path::Path::new(dir).join("Cargo.toml");
    std::fs::write(&manifest_path, cargo_manifest(&crate_name(dir)))
        .map_err(|e| format!("cannot write {}: {e}", manifest_path.display()))?;

    let opts = SplitOptions {
        funcs_per_file,
        max_bytes_per_file,
    };
    let mut write_err: Option<String> = None;
    let result = transpile_split_with_options(wasm, &opts, topts, |file| {
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
