//! Runs the core module of a `wasm32-wasip2` program natively through the minimal
//! preview2 host in `examples/wasip2_host/host.rs`.
//!
//! Usage: `cargo run --release --example wasip2_run -- <core module.wasm> [args...]`
//!
//! Get the core module of a component with `wasm2rs --extract-core component.wasm
//! dir` (the program is the largest module). The module is transpiled with
//! `named_imports`, `named_exports` and `reentrant_imports`, the host and a `main`
//! calling the `wasi:cli/run` export are appended, and the result is built with
//! `rustc -C opt-level=1` and run with `args` as the program's arguments and this
//! process's environment; its exit code is this program's. The build goes in
//! `$WASIP2_WORK_DIR` (default: a temporary directory). See
//! `docs/component-model-poc.md`.

use std::process::Command;

use wasm2rs::{TranspileOptions, transpile_with_options};
use wasmparser::{ExternalKind, Parser, Payload};

const HOST: &str = include_str!("wasip2_host/host.rs");
const RUN_EXPORT: &str = "wasi:cli/run@0.2.0#run";

include!("wasip2_host/filter.rs");

/// The index of the module's `wasi:cli/run` export.
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
        .expect("usage: wasip2_run <core module.wasm> [args...]");
    let guest_args: Vec<String> = args.collect();
    let dir = std::env::var_os("WASIP2_WORK_DIR").map_or_else(
        || std::env::temp_dir().join(format!("wasm2rs-wasip2-{}", std::process::id())),
        Into::into,
    );
    std::fs::create_dir_all(&dir).expect("create work dir");

    let wasm = std::fs::read(&input).expect("read module");
    let run = run_export(&wasm);
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
         let mut inst = Instance::new(P2Host::new(args, env));\n    \
         let status = inst.func{run}();\n    \
         std::process::exit(status);\n    \
         }}"
    );
    let src = dir.join("program.rs");
    let bin = dir.join("program");
    std::fs::write(
        &src,
        format!(
            "{generated}\n{}\n{main}\n",
            keep_declared_methods(HOST, &generated)
        ),
    )
    .expect("write source");
    let status = Command::new("rustc")
        .args(["--edition", "2024", "-C", "opt-level=1", "-A", "warnings"])
        .arg(&src)
        .arg("-o")
        .arg(&bin)
        .status()
        .expect("run rustc");
    assert!(status.success(), "rustc failed");
    let code = Command::new(&bin)
        .args(&guest_args)
        .status()
        .expect("run program")
        .code()
        .unwrap_or(1);
    std::process::exit(code);
}
