//! Runs a `wasm32-wasip2` component natively through the typed preview2 host of the
//! `wasm2rs-wasi` crate.
//!
//! Usage: `cargo run --release --example wasip2_run -- <component.wasm> [args...]`
//!
//! The program is the component's largest core module (`wasm2rs::extract_core_modules`).
//! It is transpiled with `named_imports`, `named_exports` and `reentrant_imports`; the
//! glue from `wasm2rs::wit_host_glue_with_options` (the `Glue` that lowers the library's
//! `Host` trait to the module's imports) and a `main` calling the `wasi:cli/run` export are
//! appended, and the result is built with `rustc -C opt-level=1`, linked against the
//! `wasm2rs-wasi` rlib (built by `cargo build -p wasm2rs-wasi`), and run with `args` as the
//! program's arguments and this process's environment. The guest's `argv[0]` is `<component.wasm>` as given on the command
//! line (like a real process's), followed by `args`.
//!
//! Exit codes: 0 when `run` returns ok, 1 on an error result or `wasi:cli/exit` with an
//! error, 2 when the build fails, 134 when a panic occurs during the `run` call (a guest trap,
//! or a panic inside a host import it calls: the panic message is printed, then
//! `wasip2_run: the program trapped`); panics outside the call (host setup) stay 101. The build goes in
//! `$WASIP2_WORK_DIR` (default: a temporary directory). The program sees the
//! directory `$WASIP2_PREOPEN`, if set, as its `/`: read-only, or also writable if
//! `$WASIP2_PREOPEN_WRITABLE=1`. A function the host does not implement (`wasi:http`) traps
//! when the guest calls it; an import outside the WASI 0.2.0 surface does not build (E0407)
//! and this program exits with 2. The program is dropped before a normal exit, which sends
//! what the connections' send queues still hold (up to 5 seconds); after a trap (134) it is
//! not. Addresses outside loopback stay `access-denied` (`with_external_network` is not
//! called here). See `docs/component-model-poc.md` and `docs/typed-host-design.md`.

use std::process::Command;

use wasm2rs::{
    TranspileOptions, WitOptions, extract_core_modules, transpile_with_options,
    wit_host_glue_with_options,
};
use wasmparser::{ExternalKind, Parser, Payload};

const RUN_EXPORT: &str = "wasi:cli/run@0.2.0#run";

/// The path of the `wasm2rs-wasi` rlib, built (or found up to date) by cargo, in the same
/// profile this example was built in.
fn wasi_rlib() -> std::path::PathBuf {
    let mut cargo = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    cargo.args(["build", "-p", "wasm2rs-wasi", "--message-format=json"]);
    if !cfg!(debug_assertions) {
        cargo.arg("--release");
    }
    let out = cargo
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("run cargo build -p wasm2rs-wasi");
    assert!(
        out.status.success(),
        "cargo build -p wasm2rs-wasi failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.contains("\"reason\":\"compiler-artifact\"") && l.contains("wasm2rs-wasi"))
        .filter_map(|l| {
            let at = l.find("\"filenames\":[")? + "\"filenames\":[".len();
            l[at..]
                .split('"')
                .find(|p| p.ends_with(".rlib"))
                .map(Into::into)
        })
        .next_back()
        .expect("cargo reports the wasm2rs-wasi rlib")
}

/// The index of the core module's `wasi:cli/run` export.
fn run_export(wasm: &[u8]) -> u32 {
    let mut run = None;
    for payload in Parser::new(0).parse_all(wasm) {
        if let Payload::ExportSection(reader) = payload.expect("parse module") {
            for export in reader {
                let export = export.expect("export");
                if export.name == RUN_EXPORT && export.kind == ExternalKind::Func {
                    run = Some(export.index);
                }
            }
        }
    }
    run.expect("the module does not export `wasi:cli/run@0.2.0#run`")
}

fn main() {
    let mut args = std::env::args().skip(1);
    let input = args
        .next()
        .expect("usage: wasip2_run <component.wasm> [args...]");
    let guest_args: Vec<String> = args.collect();
    let dir = std::env::var_os("WASIP2_WORK_DIR").map_or_else(
        || std::env::temp_dir().join(format!("wasm2rs-wasip2-{}", std::process::id())),
        Into::into,
    );
    std::fs::create_dir_all(&dir).expect("create work dir");

    let component = std::fs::read(&input).expect("read component");
    // The program is the largest core module (the others are adapters and shims).
    let wasm = extract_core_modules(&component)
        .expect("extract core modules")
        .into_iter()
        .max_by_key(Vec::len)
        .expect("the component has no core module");
    let run = run_export(&wasm);
    let options = WitOptions {
        external_types: Some("wasm2rs_wasi".to_string()),
        ..WitOptions::default()
    };
    let glue = wit_host_glue_with_options(&component, &options).expect("generate the host glue");
    let opts = TranspileOptions {
        named_imports: true,
        named_exports: true,
        reentrant_imports: true,
        ..TranspileOptions::default()
    };
    let generated = transpile_with_options(&wasm, &opts).expect("transpile");
    // `run` returns the lowered `result`: 0 is ok, 1 is error.
    let argv0 = &input;
    let main = format!(
        "fn main() {{\n    \
         let args = std::iter::once({argv0:?}.to_string()).chain(std::env::args_os().skip(1).map(|a| a.to_string_lossy().into_owned())).collect();\n    \
         let env = std::env::vars_os().map(|(k, v)| (k.to_string_lossy().into_owned(), v.to_string_lossy().into_owned())).collect();\n    \
         let mut host = wasm2rs_wasi::WasiHost::new(env).with_arguments(args);\n    \
         if let Some(dir) = std::env::var_os(\"WASIP2_PREOPEN\") {{\n        \
         let writable = std::env::var_os(\"WASIP2_PREOPEN_WRITABLE\").is_some_and(|v| v == \"1\");\n        \
         host = if writable {{ host.with_writable_preopen(dir.into()) }} else {{ host.with_preopen(dir.into()) }};\n    }}\n    \
         let mut inst = Instance::new(Glue(host));\n    \
         // A guest trap is a Rust panic (its message is printed by the panic hook); a panic\n    \
         // during the `run` call (also inside a host import it calls) becomes the trap exit code.\n    \
         let status = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| inst.func{run}())) {{\n        \
         Ok(status) => status,\n        \
         Err(_) => {{ eprintln!(\"wasip2_run: the program trapped\"); std::process::exit(134) }}\n    }};\n    \
         drop(inst); // flushes the connections' send queues\n    \
         std::process::exit(status);\n    \
         }}"
    );
    let src = dir.join("program.rs");
    let bin = dir.join("program");
    std::fs::write(&src, format!("{generated}\n{glue}\n{main}\n")).expect("write source");
    let rlib = wasi_rlib();
    let status = Command::new("rustc")
        .args(["--edition", "2024", "-C", "opt-level=1", "-A", "warnings"])
        .arg("--extern")
        .arg(format!("wasm2rs_wasi={}", rlib.display()))
        .arg("-L")
        .arg(format!(
            "dependency={}",
            rlib.parent()
                .map_or_else(String::new, |d| d.display().to_string())
        ))
        .arg(&src)
        .arg("-o")
        .arg(&bin)
        .status()
        .expect("run rustc");
    if !status.success() {
        eprintln!(
            "wasip2_run: rustc failed. If the errors above are E0407 (not a member of trait \
             `Host`), the component imports functions outside the WASI 0.2.0 surface that \
             wasm2rs-wasi implements."
        );
        std::process::exit(2);
    }
    let code = Command::new(&bin)
        .args(&guest_args)
        .status()
        .expect("run program")
        .code()
        .unwrap_or(1);
    std::process::exit(code);
}
