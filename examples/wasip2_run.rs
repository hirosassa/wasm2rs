//! Runs a `wasm32-wasip2` component natively through the typed preview2 host in
//! `examples/wasip2_host/typed.rs`.
//!
//! Usage: `cargo run --release --example wasip2_run -- <component.wasm> [args...]`
//!
//! The program is the component's largest core module (`wasm2rs::extract_core_modules`).
//! It is transpiled with `named_imports`, `named_exports` and `reentrant_imports`; the
//! glue from `wasm2rs::wit_host_glue(component)` (the typed `Host` trait and the `Glue`
//! that lowers it to the module's imports), the `type_aliases` of that glue, the typed
//! host (reduced to the methods the component declares) and a `main` calling the
//! `wasi:cli/run` export are appended, and the result is built with
//! `rustc -C opt-level=1` and run with `args` as the program's arguments and this
//! process's environment; its exit code is this program's (0 when `run` returns ok, 1
//! on an error result or `wasi:cli/exit` with an error). The build goes in
//! `$WASIP2_WORK_DIR` (default: a temporary directory). The program sees the
//! directory `$WASIP2_PREOPEN`, if set, as its `/`: read-only, or also writable if
//! `$WASIP2_PREOPEN_WRITABLE=1`. A component that imports a function `typed.rs` does
//! not implement does not build: rustc's errors are shown (E0046, missing trait items)
//! and this program exits with 2. See `docs/component-model-poc.md`.

use std::process::Command;

use wasm2rs::{TranspileOptions, extract_core_modules, transpile_with_options, wit_host_glue};
use wasmparser::{ExternalKind, Parser, Payload};

const HOST: &str = include_str!("wasip2_host/typed.rs");
const RUN_EXPORT: &str = "wasi:cli/run@0.2.0#run";

include!("wasip2_host/filter.rs");

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
    let glue = wit_host_glue(&component).expect("generate the host glue");
    let opts = TranspileOptions {
        named_imports: true,
        named_exports: true,
        reentrant_imports: true,
        ..TranspileOptions::default()
    };
    let generated = transpile_with_options(&wasm, &opts).expect("transpile");
    // `run` returns the lowered `result`: 0 is ok, 1 is error.
    let main = format!(
        "fn main() {{\n    \
         let args = std::env::args_os().map(|a| a.to_string_lossy().into_owned()).collect();\n    \
         let env = std::env::vars_os().map(|(k, v)| (k.to_string_lossy().into_owned(), v.to_string_lossy().into_owned())).collect();\n    \
         let mut host = TypedHost::new(env).with_arguments(args);\n    \
         if let Some(dir) = std::env::var_os(\"WASIP2_PREOPEN\") {{\n        \
         let writable = std::env::var_os(\"WASIP2_PREOPEN_WRITABLE\").is_some_and(|v| v == \"1\");\n        \
         host = if writable {{ host.with_writable_preopen(dir.into()) }} else {{ host.with_preopen(dir.into()) }};\n    }}\n    \
         let mut inst = Instance::new(Glue(host));\n    \
         let status = inst.func{run}();\n    \
         std::process::exit(status);\n    \
         }}"
    );
    let aliases = type_aliases(&glue);
    let host = keep_declared_methods(HOST, &glue);
    let src = dir.join("program.rs");
    let bin = dir.join("program");
    std::fs::write(
        &src,
        format!("{generated}\n{glue}\n{aliases}\n{host}\n{main}\n"),
    )
    .expect("write source");
    let status = Command::new("rustc")
        .args(["--edition", "2024", "-C", "opt-level=1", "-A", "warnings"])
        .arg(&src)
        .arg("-o")
        .arg(&bin)
        .status()
        .expect("run rustc");
    if !status.success() {
        eprintln!(
            "wasip2_run: rustc failed. If the errors above are E0046 (missing trait items), the \
             component imports functions that examples/wasip2_host/typed.rs does not implement."
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
