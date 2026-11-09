//! Runs a `wasm32-wasip2` component natively through the typed preview2 host of the
//! `wasm2rs-wasi` crate.
//!
//! Usage: `cargo run --release --example wasip2_run -- <component.wasm> [args...]`
//!
//! The program is the component's largest core module (`wasm2rs::extract_core_modules`).
//! It is transpiled with `named_imports`, `named_exports` and `reentrant_imports`; the
//! glue from `wasm2rs::wit_host_glue_with_options` (the `Glue` that lowers the library's
//! `Host` trait to the module's imports, and with `exports` the typed `Exports` trait) and a
//! `main` calling the typed `wasi:cli/run` export are appended, and the result is built with
//! `rustc -C opt-level=1`, linked against the `wasm2rs-wasi` library (built with `cargo` into
//! the work directory first), and run with `args` as the program's arguments and this
//! process's environment. The guest's `argv[0]` is `<component.wasm>` as given on the command
//! line (like a real process's), followed by `args`.
//!
//! Exit codes: 0 when `run` returns ok, 1 on an error result or `wasi:cli/exit` with an
//! error, 2 when the build fails or the component does not lift `wasi:cli/run@0.2.0#run`
//! directly (no such export, or a shape `wasm2rs::wit_exports` does not read: the message says
//! which), 134 when a panic occurs during the `run` call (a guest trap, or a panic inside a
//! host import it calls: the panic message is printed, then `wasip2_run: the program
//! trapped`); panics outside the call (host setup) stay 101. The build goes in
//! `$WASIP2_WORK_DIR` (default: a temporary directory). The program sees the
//! directory `$WASIP2_PREOPEN`, if set, as its `/`: read-only, or also writable if
//! `$WASIP2_PREOPEN_WRITABLE=1`. A function the host does not implement (`wasi:http`) traps
//! when the guest calls it; an import outside the WASI 0.2.0 surface does not build (E0407)
//! and this program exits with 2. The program is dropped before a normal exit, which sends
//! what the connections' send queues still hold (up to 5 seconds); after a trap (134) it is
//! not. Addresses outside loopback stay `access-denied` (`with_external_network` is not
//! called here). See `docs/typed-host-design.md` and `docs/component-exports-design.md`.

use std::process::Command;

use wasm2rs::{
    TranspileOptions, WitExport, WitOptions, extract_core_modules, transpile_with_options,
    wit_exports, wit_host_glue_with_options,
};

const RUN_INTERFACE: &str = "wasi:cli/run@0.2.0";

/// Builds the `wasm2rs-wasi` library with `cargo` into `dir` (it depends on `getrandom`, which a
/// bare `rustc` call cannot resolve): the rlib and the directory of its dependencies' rlibs.
fn build_wasi_rlib(
    dir: &std::path::Path,
) -> (
    std::path::PathBuf,
    Option<std::path::PathBuf>,
    Vec<std::path::PathBuf>,
) {
    let target = dir.join("wasi-target");
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let out = Command::new(cargo)
        .args([
            "build",
            "--release",
            "-p",
            "wasm2rs-wasi",
            "--message-format=json-render-diagnostics",
            "--manifest-path",
        ])
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
        .arg("--target-dir")
        .arg(&target)
        .stderr(std::process::Stdio::inherit())
        .output()
        .expect("run cargo");
    assert!(out.status.success(), "wasm2rs-wasi failed to build");
    // cargo says where it put the rlibs (it depends on the version): the library, and the
    // directories to search for its dependencies.
    let mut lib = None;
    let mut rmeta = None;
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if !line.contains("\"reason\":\"compiler-artifact\"") {
            continue;
        }
        let Some(list) = line
            .split("\"filenames\":[")
            .nth(1)
            .and_then(|l| l.split(']').next())
        else {
            continue;
        };
        if line.contains("\"name\":\"wasm2rs_wasi\"") {
            // newer cargo leaves only a metadata stub in the rlib: the .rmeta must be given too
            rmeta = list
                .split('"')
                .find(|f| f.ends_with(".rmeta"))
                .map(std::path::PathBuf::from);
        }
        for rlib in list.split('"').filter(|f| f.ends_with(".rlib")) {
            let rlib = std::path::PathBuf::from(rlib);
            if line.contains("\"name\":\"wasm2rs_wasi\"") {
                lib = Some(rlib.clone());
            }
            if let Some(dir) = rlib.parent().filter(|d| !dirs.iter().any(|x| x == d)) {
                dirs.push(dir.to_path_buf());
            }
        }
    }
    (
        lib.expect("cargo reported no wasm2rs-wasi rlib"),
        rmeta,
        dirs,
    )
}

fn is_run(export: &WitExport) -> bool {
    export.func.interface == RUN_INTERFACE && export.func.name == "run"
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
    // `run` must be lifted directly by the component: anything else (a missing export, an
    // adapter in between) is reported here, before the build.
    match wit_exports(&component) {
        Ok(exports) if exports.iter().any(is_run) => {}
        Ok(_) => {
            eprintln!("wasip2_run: the component does not export `{RUN_INTERFACE}#run`");
            std::process::exit(2);
        }
        Err(err) => {
            eprintln!("wasip2_run: {err}");
            std::process::exit(2);
        }
    }
    let options = WitOptions {
        external_types: Some("wasm2rs_wasi".to_string()),
        exports: true,
        ..WitOptions::default()
    };
    // `exports: true` generates a method for every export of the component, not only `run`:
    // one the glue cannot lower or lift fails here too.
    let glue = wit_host_glue_with_options(&component, &options).unwrap_or_else(|err| {
        eprintln!("wasip2_run: {err}");
        std::process::exit(2);
    });
    let opts = TranspileOptions {
        named_imports: true,
        named_exports: true,
        reentrant_imports: true,
        ..TranspileOptions::default()
    };
    let generated = transpile_with_options(&wasm, &opts).expect("transpile");
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
    let src = dir.join("program.rs");
    let bin = dir.join("program");
    std::fs::write(&src, format!("{generated}\n{glue}\n{main}\n")).expect("write source");
    let (rlib, rmeta, deps) = build_wasi_rlib(&dir);
    let status = Command::new("rustc")
        .args(["--edition", "2024", "-C", "opt-level=1", "-A", "warnings"])
        .arg("--extern")
        .arg(format!("wasm2rs_wasi={}", rlib.display()))
        .args(rmeta.iter().flat_map(|m| {
            [
                "--extern".to_string(),
                format!("wasm2rs_wasi={}", m.display()),
            ]
        }))
        .args(
            deps.iter()
                .flat_map(|d| ["-L".to_string(), format!("dependency={}", d.display())]),
        )
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
