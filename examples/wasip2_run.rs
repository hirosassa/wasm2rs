//! Runs a `wasm32-wasip2` component natively through the typed preview2 host of the
//! `wasm2rs-wasi` crate.
//!
//! Usage: `cargo run --release --example wasip2_run -- <component.wasm> [args...]`
//!
//! `wasm2rs::transpile_component` turns the component into one Rust source: the program core
//! module (the one that lifts the component's exports), transpiled with `named_imports`,
//! `named_exports` and `reentrant_imports`, then the `Glue` that lowers the library's `Host`
//! trait to the module's imports and the typed `Exports` trait. A `main` calling the typed
//! `wasi:cli/run` export is appended, and the result is built with
//! `rustc -C opt-level=1`, linked against the `wasm2rs-wasi` library (built with `cargo` into
//! the work directory first), and run with `args` as the program's arguments and this
//! process's environment. The guest's `argv[0]` is `<component.wasm>` as given on the command
//! line (like a real process's), followed by `args`.
//!
//! Exit codes: 0 when `run` returns ok, 1 on an error result or `wasi:cli/exit` with an
//! error, 2 when the build fails or the component does not lift `wasi:cli/run@0.2.0#run`
//! directly (no such export, or a shape `wasm2rs::transpile_component` does not read: the
//! message says which), 134 when a panic occurs during the `run` call (a guest trap, or a
//! panic inside a host import it calls: the panic message is printed, then `wasip2_run: the
//! program trapped`); panics outside the call (host setup) stay 101. The build goes in
//! `$WASIP2_WORK_DIR` (default: a temporary directory). The program sees the
//! directory `$WASIP2_PREOPEN`, if set, as its `/`: read-only, or also writable if
//! `$WASIP2_PREOPEN_WRITABLE=1`. `wasi:http` is implemented, but no
//! `HttpClient` is set here (`UreqClient` needs the `http-client` feature), so
//! `outgoing-handler.handle` fails with `internal-error`. An import outside the WASI 0.2.0
//! surface does not build (E0407) and this program exits with 2. The program is dropped before a normal exit, which sends
//! what the connections' send queues still hold (up to 5 seconds); after a trap (134) it is
//! not. Addresses outside loopback stay `access-denied` (`with_external_network` is not
//! called here). See `docs/typed-host-design.md` and `docs/component-exports-design.md`.

use std::process::Command;

mod common;

const RUN_INTERFACE: &str = "wasi:cli/run@0.2.0";

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
    // `wasm2rs::transpile_component` picks the program module and generates the glue.
    // The default `ComponentOptions` (`exports: true`) make a method for every export of the
    // component, not only `run`: one the glue cannot lower or lift fails here too.
    let generated =
        common::transpile_exporting(&component, RUN_INTERFACE, "run").unwrap_or_else(|err| {
            eprintln!("wasip2_run: {err}");
            std::process::exit(2);
        });
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
         let result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| Exports::export_wasi_cli_run_0_2_0_run(&mut inst))) {{\n        \
         Ok(result) => result,\n        \
         Err(_) => {{ eprintln!(\"wasip2_run: the program trapped\"); std::process::exit(134) }}\n    }};\n    \
         drop(inst); // flushes the connections' send queues\n    \
         std::process::exit(if result.is_ok() {{ 0 }} else {{ 1 }});\n    \
         }}"
    );
    let source = format!("{generated}\n{main}\n");
    let Some(bin) = common::build_program(&dir, &source) else {
        eprintln!(
            "wasip2_run: rustc failed. If the errors above are E0407 (not a member of trait \
             `Host`), the component imports functions outside the WASI 0.2.0 surface that \
             wasm2rs-wasi implements."
        );
        std::process::exit(2);
    };
    let code = Command::new(&bin)
        .args(&guest_args)
        .status()
        .expect("run program")
        .code()
        .unwrap_or(1);
    std::process::exit(code);
}
