//! Command-line entry point:
//! `wasm2rs [--unsafe-memory] [--jobs=N] [--split-dispatch=N] [--entry-exports=a,b] [--named-exports] [--named-imports]
//! <input.wasm> [output] [funcs_per_file] [max_bytes_per_file]`, or
//! `wasm2rs --dump <input.wasm>` to summarize a module, or
//! `wasm2rs --extract-core <component.wasm> <dir>` to write a component's core
//! modules to `<dir>/module{n}.wasm`, or
//! `wasm2rs --wit-host [--glue [--external-types=<path>]] <component.wasm> [output]` to write a
//! typed `Host` trait for the interfaces the component imports (with `--glue`, also the `Glue`
//! that lowers it to the flat `Imports` trait; with `--external-types=<path>`, only the `Glue`,
//! naming the `Host` trait and the interface-qualified types of the library at `<path>`).
//!
//! `wasm2rs --component [--unsafe-memory] [--jobs=N] [--split-dispatch=N] [--external-types=<path>]
//! <component.wasm> [output]` to write a component's program core module plus the typed `Glue`
//! and `Exports` (like `wasm2rs::transpile_component`).
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

const USAGE: &str = "usage: wasm2rs [--unsafe-memory] [--jobs=N] [--split-dispatch=N] [--entry-exports=a,b] [--named-exports] [--named-imports] [--import-memory] [--reentrant-imports] <input.wasm> [output] [funcs_per_file] [max_bytes_per_file]\n       wasm2rs --component [--unsafe-memory] [--jobs=N] [--split-dispatch=N] [--external-types=<path>] <component.wasm> [output]\n       wasm2rs --dump <input.wasm>\n       wasm2rs --extract-core <component.wasm> <output dir>\n       wasm2rs --wit-host [--glue [--external-types=<path>]] <component.wasm> [output]";

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

/// Write each core module of the component to `dir/module{n}.wasm` and list them.
fn extract_core_modules(wasm: &[u8], dir: &str) -> Result<(), String> {
    let modules = wasm2rs::extract_core_modules(wasm).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {dir}: {e}"))?;
    for (i, module) in modules.iter().enumerate() {
        let path = std::path::Path::new(dir).join(format!("module{i}.wasm"));
        std::fs::write(&path, module)
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        println!("{}: {} bytes", path.display(), module.len());
    }
    Ok(())
}

/// Write `text` to the `output` file, or to stdout when there is none.
fn write_output(output: Option<String>, text: &str) -> Result<(), String> {
    match output {
        Some(path) => std::fs::write(&path, text).map_err(|e| format!("cannot write {path}: {e}")),
        None => std::io::stdout()
            .write_all(text.as_bytes())
            .map_err(|e| format!("cannot write to stdout: {e}")),
    }
}

fn run() -> Result<(), String> {
    // Flags may appear anywhere; the remaining positional arguments keep their
    // order. `--unsafe-memory` opts into unchecked linear-memory access.
    let mut unsafe_memory = false;
    let mut dump = false;
    let mut extract_core = false;
    let mut component = false;
    let mut wit_host = false;
    let mut glue = false;
    let mut external_types: Option<String> = None;
    let mut named_exports = false;
    let mut named_imports = false;
    let mut import_memory = false;
    let mut reentrant_imports = false;
    let mut entry_exports: Option<Vec<String>> = None;
    let mut split_dispatch = 0usize;
    let mut jobs = 0usize;
    let mut positional: Vec<String> = Vec::new();
    for arg in std::env::args().skip(1) {
        if let Some(n) = arg.strip_prefix("--split-dispatch=") {
            split_dispatch = n
                .parse()
                .map_err(|_| format!("invalid --split-dispatch value: {n}"))?;
            continue;
        }
        if let Some(n) = arg.strip_prefix("--jobs=") {
            jobs = n
                .parse()
                .map_err(|_| format!("invalid --jobs value: {n}"))?;
            continue;
        }
        if let Some(path) = arg.strip_prefix("--external-types=") {
            if path.is_empty() {
                return Err(
                    "--external-types needs a Rust path, e.g. --external-types=my_host::wasi"
                        .to_string(),
                );
            }
            external_types = Some(path.to_string());
            continue;
        }
        if let Some(list) = arg.strip_prefix("--entry-exports=") {
            entry_exports = Some(
                list.split(',')
                    .filter(|name| !name.is_empty())
                    .map(str::to_string)
                    .collect(),
            );
            continue;
        }
        match arg.as_str() {
            "--unsafe-memory" => unsafe_memory = true,
            "--dump" => dump = true,
            "--extract-core" => extract_core = true,
            "--component" => component = true,
            "--wit-host" => wit_host = true,
            "--glue" => glue = true,
            "--named-exports" => named_exports = true,
            "--named-imports" => named_imports = true,
            "--import-memory" => import_memory = true,
            "--reentrant-imports" => reentrant_imports = true,
            _ => positional.push(arg),
        }
    }
    let mut args = positional.into_iter();
    let input = args.next().ok_or(USAGE)?;
    let output = args.next();
    let funcs_per_file = parse_usize_arg(args.next(), "funcs_per_file")?;
    let max_bytes_per_file = parse_usize_arg(args.next(), "max_bytes_per_file")?;

    // `--dump`, `--extract-core` and `--wit-host` generate no code, so they refuse extra
    // arguments and the flags that only change generated code instead of silently
    // ignoring them.
    let refuse_extras = |mode: &str, takes: &str, extra_args: bool| {
        if extra_args {
            return Err(format!("{mode} takes only {takes}"));
        }
        let code_flags = [
            (unsafe_memory, "--unsafe-memory"),
            (named_exports, "--named-exports"),
            (named_imports, "--named-imports"),
            (import_memory, "--import-memory"),
            (reentrant_imports, "--reentrant-imports"),
            (split_dispatch > 0, "--split-dispatch"),
            (jobs > 0, "--jobs"),
            (entry_exports.is_some(), "--entry-exports"),
            // The one code flag that `--wit-host --glue` itself takes.
            (external_types.is_some() && !wit_host, "--external-types"),
        ];
        match code_flags
            .into_iter()
            .find_map(|(given, flag)| given.then_some(flag))
        {
            Some(flag) => Err(format!(
                "{mode} takes only {takes}; {flag} changes the generated code, which {mode} does not generate"
            )),
            None => Ok(()),
        }
    };

    if component {
        // `--component` sets the naming/reentrancy/glue flags itself and writes one source,
        // so a flag it would override or ignore is refused by name rather than dropped.
        let refused = [
            (dump, "--dump"),
            (extract_core, "--extract-core"),
            (wit_host, "--wit-host"),
            (glue, "--glue"),
            (named_exports, "--named-exports"),
            (named_imports, "--named-imports"),
            (reentrant_imports, "--reentrant-imports"),
            (import_memory, "--import-memory"),
            (entry_exports.is_some(), "--entry-exports"),
            (
                funcs_per_file > 0 || max_bytes_per_file > 0,
                "funcs_per_file/max_bytes_per_file",
            ),
        ];
        if let Some((_, flag)) = refused.into_iter().find(|(given, _)| *given) {
            return Err(format!("--component cannot be combined with {flag}"));
        }
        let wasm = std::fs::read(&input).map_err(|e| format!("cannot read {input}: {e}"))?;
        let defaults = wasm2rs::ComponentOptions::default();
        let options = wasm2rs::ComponentOptions {
            transpile: TranspileOptions {
                unsafe_memory,
                split_dispatch,
                jobs,
                ..TranspileOptions::default()
            },
            external_types: external_types.or(defaults.external_types),
            ..defaults
        };
        let rust = wasm2rs::transpile_component(&wasm, &options).map_err(|e| e.to_string())?;
        return write_output(output, &rust);
    }
    if glue && !wit_host {
        return Err("--glue only applies to --wit-host".to_string());
    }
    if external_types.is_some() && !glue {
        return Err("--external-types only applies to --wit-host --glue".to_string());
    }
    let wasm = std::fs::read(&input).map_err(|e| format!("cannot read {input}: {e}"))?;
    if wit_host {
        for (given, flag) in [(dump, "--dump"), (extract_core, "--extract-core")] {
            if given {
                return Err(format!("--wit-host cannot be combined with {flag}"));
            }
        }
        refuse_extras(
            "--wit-host",
            "the input file and an optional output file",
            funcs_per_file > 0 || max_bytes_per_file > 0,
        )?;
        let rust = if glue {
            let options = wasm2rs::WitOptions {
                external_types,
                ..wasm2rs::WitOptions::default()
            };
            wasm2rs::wit_host_glue_with_options(&wasm, &options)
        } else {
            wasm2rs::wit_host_trait(&wasm)
        }
        .map_err(|e| e.to_string())?;
        return write_output(output, &rust);
    }
    if extract_core {
        if dump {
            return Err("--extract-core cannot be combined with --dump".to_string());
        }
        let dir = output.ok_or("--extract-core takes <input.wasm> <output dir>")?;
        refuse_extras(
            "--extract-core",
            "the input file and the output directory",
            funcs_per_file > 0 || max_bytes_per_file > 0,
        )?;
        return extract_core_modules(&wasm, &dir);
    }
    if dump {
        refuse_extras(
            "--dump",
            "the input file",
            output.is_some() || funcs_per_file > 0 || max_bytes_per_file > 0,
        )?;
        let summary = wasm2rs::dump(&wasm).map_err(|e| e.to_string())?;
        print!("{summary}");
        return Ok(());
    }
    let topts = TranspileOptions {
        unsafe_memory,
        split_dispatch,
        entry_exports,
        jobs,
        named_exports,
        named_imports,
        import_memory,
        reentrant_imports,
    };

    // Splitting into files requires a target directory to write them into.
    if funcs_per_file > 0 || max_bytes_per_file > 0 {
        let dir = output.ok_or("splitting requires an output directory")?;
        return write_split(&wasm, &dir, funcs_per_file, max_bytes_per_file, &topts);
    }

    let rust = transpile_with_options(&wasm, &topts).map_err(|e| e.to_string())?;
    write_output(output, &rust)
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
