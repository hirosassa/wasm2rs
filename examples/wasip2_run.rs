//! Runs the core module of a `wasm32-wasip2` program natively through the minimal
//! preview2 host in `examples/wasip2_host/host.rs`.
//!
//! Usage: `cargo run --release --example wasip2_run -- <core module.wasm> [work dir]`
//!
//! Get the core module of a component with `wasm2rs --extract-core component.wasm
//! dir` (the program is the largest module). The module is transpiled with
//! `named_imports` and `import_memory`, the host and a `main` calling the
//! `wasi:cli/run` export are appended, and the result is built with `rustc -C opt-level=1` and
//! run; its exit code is this program's. See `docs/component-model-poc.md`.

use std::process::Command;

use wasm2rs::{TranspileOptions, transpile_with_options};
use wasmparser::{ExternalKind, Parser, Payload};

const HOST: &str = include_str!("wasip2_host/host.rs");
const RUN_EXPORT: &str = "wasi:cli/run@0.2.0#run";

include!("wasip2_host/filter.rs");

/// The module's declared minimum pages for memory 0 and the index of its
/// `wasi:cli/run` export.
fn inspect(wasm: &[u8]) -> (u64, u32) {
    let (mut pages, mut run) = (None, None);
    for payload in Parser::new(0).parse_all(wasm) {
        match payload.expect("parse module") {
            Payload::MemorySection(reader) => {
                pages = pages.or(reader
                    .into_iter()
                    .next()
                    .map(|m| m.expect("memory").initial));
            }
            Payload::ExportSection(reader) => {
                for export in reader {
                    let export = export.expect("export");
                    if export.name == RUN_EXPORT && export.kind == ExternalKind::Func {
                        run = Some(export.index);
                    }
                }
            }
            _ => {}
        }
    }
    (
        pages.expect("the module has no memory"),
        run.expect("the module does not export `wasi:cli/run@0.2.0#run`"),
    )
}

fn main() {
    let mut args = std::env::args().skip(1);
    let input = args
        .next()
        .expect("usage: wasip2_run <core module.wasm> [work dir]");
    let dir = args.next().map_or_else(
        || std::env::temp_dir().join(format!("wasm2rs-wasip2-{}", std::process::id())),
        Into::into,
    );
    std::fs::create_dir_all(&dir).expect("create work dir");

    let wasm = std::fs::read(&input).expect("read module");
    let (pages, run) = inspect(&wasm);
    let opts = TranspileOptions {
        named_imports: true,
        import_memory: true,
        ..TranspileOptions::default()
    };
    let generated = transpile_with_options(&wasm, &opts).expect("transpile");
    // `run` returns the lowered `result`: 0 is ok, 1 is error.
    let main = format!(
        "fn main() {{\n    \
         let mut inst = Instance::new(P2Host::new({pages}));\n    \
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
        .status()
        .expect("run program")
        .code()
        .unwrap_or(1);
    std::process::exit(code);
}
