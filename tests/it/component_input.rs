//! A WebAssembly *component* is rejected with a clear error instead of being
//! mis-parsed as a core module.

use std::process::Command;

use wasm2rs::{
    ComponentOptions, TranspileError, TranspileOptions, dump, transpile, transpile_component,
    transpile_component_with_exports, wit_exports,
};

use crate::common;
use crate::wasip2_run_example::{CORE, WORLD, component_of};

fn component() -> Vec<u8> {
    wat::parse_str("(component (core module (func (export \"f\"))))").unwrap()
}

#[test]
fn transpile_rejects_a_component() {
    let err = transpile(&component()).unwrap_err().to_string();
    assert!(err.contains("component"), "{err}");
}

#[test]
fn dump_rejects_a_component() {
    let err = dump(&component()).map(|_| ()).unwrap_err().to_string();
    assert!(err.contains("component"), "{err}");
}

#[test]
fn the_rejection_points_at_the_component_entry_points() {
    let err = transpile(&component()).unwrap_err().to_string();
    assert!(err.contains("--component"), "{err}");
    assert!(err.contains("transpile_component"), "{err}");
}

#[test]
fn a_core_module_still_transpiles() {
    let wasm = wat::parse_str("(module (func (export \"f\")))").unwrap();
    assert!(transpile(&wasm).is_ok());
    assert!(dump(&wasm).is_ok());
}

#[test]
fn the_cli_fails_on_a_component() {
    let dir = std::env::temp_dir().join(format!("wasm2rs_component_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("c.wasm");
    std::fs::write(&input, component()).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_wasm2rs"))
        .arg(&input)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("component"), "{stderr}");
}

// --- `transpile_component` ---

#[allow(clippy::panic, reason = "test code")]
fn unsupported(wasm: &[u8]) -> String {
    match transpile_component(wasm, &ComponentOptions::default()) {
        Err(TranspileError::InvalidInput(why)) => why,
        other => panic!("expected InvalidInput, got {other:?}"),
    }
}

#[test]
fn a_run_component_yields_the_module_the_glue_and_the_exports() {
    let code =
        transpile_component(&component_of(WORLD, CORE), &ComponentOptions::default()).unwrap();
    assert!(code.contains("pub struct Instance"), "core module missing");
    assert!(code.contains("pub struct Glue"), "glue missing");
    assert!(code.contains("pub trait Exports"), "exports missing");
    assert!(code.contains("use wasm2rs_wasi::Host;"), "external types");
}

#[test]
fn with_exports_matches_transpile_component_and_wit_exports() {
    let opts = ComponentOptions::default();
    let component = component_of(WORLD, CORE);
    let (code, exports) = transpile_component_with_exports(&component, &opts).unwrap();
    assert_eq!(code, transpile_component(&component, &opts).unwrap());
    assert_eq!(exports, wit_exports(&component).unwrap());
    assert!(!exports.is_empty(), "the run component has an export");
}

#[test]
fn with_exports_of_a_component_without_exports_is_empty() {
    let opts = ComponentOptions::default();
    let component = wat::parse_str("(component (core module (func)))").unwrap();
    let (code, exports) = transpile_component_with_exports(&component, &opts).unwrap();
    assert_eq!(code, transpile_component(&component, &opts).unwrap());
    assert!(exports.is_empty());
}

#[test]
fn the_generated_source_builds_and_runs_the_typed_run_export() {
    let code =
        transpile_component(&component_of(WORLD, CORE), &ComponentOptions::default()).unwrap();
    let out = build_and_run(&code, "component_input_run", "hello");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "hello");
    assert!(out.status.success());
}

/// Build `code` (a transpiled `run` component) with a `main` that calls its typed `run`
/// export with the argument `arg`, run it and return its output.
pub(crate) fn build_and_run(code: &str, name: &str, arg: &str) -> std::process::Output {
    let program = format!(
        "{code}\nfn main() {{\n    \
         let host = wasm2rs_wasi::WasiHost::new(Vec::new()).with_arguments(vec![\"p\".to_string(), \"{arg}\".to_string()]);\n    \
         let mut inst = Instance::new(Glue(host));\n    \
         let result = Exports::export_wasi_cli_run_0_2_0_run(&mut inst);\n    \
         drop(inst);\n    \
         std::process::exit(if result.is_ok() {{ 0 }} else {{ 1 }});\n}}\n"
    );
    let bin = common::build_with_wasi(name, &program);
    Command::new(bin).output().unwrap()
}

#[test]
fn the_options_the_glue_needs_are_forced_on() {
    let options = ComponentOptions {
        transpile: TranspileOptions::default()
            .with_named_imports(false)
            .with_named_exports(false)
            .with_reentrant_imports(false),
        ..ComponentOptions::default()
    };
    let code = transpile_component(&component_of(WORLD, CORE), &options).unwrap();
    // named_exports: the `Exports` impl calls the core export by its name.
    assert!(
        code.contains("pub fn export_wasi_cli_run_0_2_0_run(&mut self)"),
        "named_exports"
    );
    // reentrant_imports: the host gets the instance back.
    assert!(code.contains("g: &mut Instance"), "reentrant_imports");
}

#[test]
fn exports_can_be_turned_off() {
    let options = ComponentOptions {
        exports: false,
        ..ComponentOptions::default()
    };
    let code = transpile_component(&component_of(WORLD, CORE), &options).unwrap();
    assert!(code.contains("pub struct Glue"));
    assert!(!code.contains("pub trait Exports"));
}

#[test]
fn without_external_types_the_glue_declares_its_own_host() {
    let options = ComponentOptions {
        external_types: None,
        ..ComponentOptions::default()
    };
    let code = transpile_component(&component_of(WORLD, CORE), &options).unwrap();
    assert!(code.contains("pub trait Host"));
}

/// Only imports (`get-arguments` needs memory, so wit-component adds a shim and a fixup
/// module around the program): the program is the module that receives the lowered functions.
#[test]
fn an_import_only_component_selects_the_module_that_receives_the_lowered_imports() {
    let world = r#"
package test:t;
world w {
  import wasi:cli/environment@0.2.0;
}
"#;
    let core = r#"
(module
  (import "wasi:cli/environment@0.2.0" "get-arguments" (func $args (param i32)))
  (memory (export "memory") 1)
  (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32) (i32.const 0))
  (func (export "marker_program") (call $args (i32.const 0))))
"#;
    let code =
        transpile_component(&component_of(world, core), &ComponentOptions::default()).unwrap();
    assert!(code.contains("fn marker_program"), "the program module");
    assert!(code.contains("pub struct Glue"));
}

fn wat_component(text: &str) -> Vec<u8> {
    wat::parse_str(text).unwrap()
}

/// Two core modules; the one the export lifts is neither the first nor the largest.
#[test]
fn the_module_named_by_the_lift_wins_over_a_larger_one() {
    let component = wat_component(
        r#"
(component
  (core module $adapter
    (func (export "filler") (result i32)
      i32.const 1 i32.const 2 i32.add i32.const 3 i32.add i32.const 4 i32.add))
  (core module $program
    (func (export "marker_program") (result i32) i32.const 7))
  (core instance $p (instantiate $program))
  (func $f (result u32) (canon lift (core func $p "marker_program")))
  (export "marker" (func $f)))
"#,
    );
    let code = transpile_component(&component, &ComponentOptions::default()).unwrap();
    assert!(code.contains("fn marker_program"), "{code}");
    assert!(!code.contains("fn filler"), "the other module leaked in");
}

/// A nested component's module precedes the program: indices must still line up.
#[test]
fn a_nested_components_module_does_not_shift_the_selection() {
    let component = wat_component(
        r#"
(component
  (component $inner
    (core module (func (export "inner_marker"))))
  (core module $program
    (func (export "marker_program") (result i32) i32.const 7))
  (core instance $p (instantiate $program))
  (func $f (result u32) (canon lift (core func $p "marker_program")))
  (export "marker" (func $f)))
"#,
    );
    let code = transpile_component(&component, &ComponentOptions::default()).unwrap();
    assert!(code.contains("fn marker_program"), "{code}");
    assert!(!code.contains("inner_marker"));
}

/// A core module exported before the program takes an index of its own: the instantiation of
/// the program (index 2) must still be found.
#[test]
fn an_exported_module_shifts_the_index_space_like_the_spec_says() {
    let component = wat_component(
        r#"
(component
  (import "test:t/i" (instance $i (export "f" (func))))
  (alias export $i "f" (func $f))
  (core module $fixup (import "env" "f" (func)))
  (export "m" (core module $fixup))
  (core module $program (import "env" "f" (func)) (func (export "marker_program")))
  (core func $l (canon lower (func $f)))
  (core instance $env (export "f" (func $l)))
  (core instance (instantiate $program (with "env" (instance $env)))))
"#,
    );
    let code = transpile_component(&component, &ComponentOptions::default()).unwrap();
    assert!(code.contains("fn marker_program"), "{code}");
}

#[test]
fn an_undecidable_component_is_unsupported_with_the_reason() {
    let component = wat_component(
        r#"
(component
  (import "test:t/i" (instance $i (export "f" (func))))
  (alias export $i "f" (func $f))
  (core module $a (import "env" "f" (func)) (func (export "a")))
  (core module $b (import "env" "f" (func)) (func (export "b")))
  (core func $l (canon lower (func $f)))
  (core instance $env (export "f" (func $l)))
  (core instance (instantiate $a (with "env" (instance $env))))
  (core instance (instantiate $b (with "env" (instance $env)))))
"#,
    );
    let why = unsupported(&component);
    assert!(why.contains("more than one core module"), "{why}");
}

#[test]
fn a_component_without_a_core_module_is_unsupported() {
    let component = wat_component("(component)");
    let why = unsupported(&component);
    assert!(why.contains("no core module"), "{why}");
}

#[test]
fn a_core_module_input_says_to_use_transpile() {
    let wasm = wat::parse_str("(module (func (export \"f\")))").unwrap();
    let why = unsupported(&wasm);
    assert!(
        why.contains("core module") && why.contains("transpile"),
        "{why}"
    );
}

#[test]
fn broken_input_is_an_error_not_a_panic() {
    let good = component_of(WORLD, CORE);
    let mut inputs: Vec<Vec<u8>> = vec![
        Vec::new(),
        b"not wasm".to_vec(),
        vec![0, b'a', b's', b'm', 0x0d, 0, 1, 0],
    ];
    for cut in [8, 20, good.len() / 2, good.len() - 1] {
        inputs.push(good[..cut].to_vec());
    }
    for input in inputs {
        assert!(
            transpile_component(&input, &ComponentOptions::default()).is_err(),
            "{} bytes",
            input.len()
        );
    }
}

// --- the CLI's `--component` ---

/// Write `wasm` to `c.wasm` in a fresh directory and return `(dir, input)`.
fn cli_input(name: &str, wasm: &[u8]) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "wasm2rs_cli_component_{name}_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("c.wasm");
    std::fs::write(&input, wasm).unwrap();
    (dir, input)
}

/// [`cli_input`] with the `run` component.
fn cli_component(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    cli_input(name, &component_of(WORLD, CORE))
}

fn cli(args: &[&std::ffi::OsStr]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_wasm2rs"))
        .args(args)
        .output()
        .unwrap()
}

/// Run the CLI, assert it succeeded (showing its stderr otherwise) and return its stdout.
fn cli_ok(args: &[&std::ffi::OsStr]) -> String {
    let out = cli(args);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn the_cli_component_flag_writes_what_transpile_component_returns() {
    let (dir, input) = cli_component("same");
    let output = dir.join("out.rs");
    cli_ok(&[
        "--component".as_ref(),
        input.as_os_str(),
        output.as_os_str(),
    ]);
    let expected =
        transpile_component(&component_of(WORLD, CORE), &ComponentOptions::default()).unwrap();
    assert_eq!(std::fs::read_to_string(output).unwrap(), expected);
}

#[test]
fn the_cli_component_flag_writes_to_stdout_without_an_output() {
    let (_dir, input) = cli_component("stdout");
    let stdout = cli_ok(&["--component".as_ref(), input.as_os_str()]);
    assert!(stdout.contains("pub struct Glue"), "glue missing");
    assert!(stdout.contains("pub trait Exports"), "exports missing");
}

#[test]
fn the_cli_component_output_builds_and_runs() {
    let (_dir, input) = cli_component("run");
    let code = cli_ok(&["--component".as_ref(), input.as_os_str()]);
    let run = build_and_run(&code, "cli_component_run", "hi");
    assert_eq!(String::from_utf8_lossy(&run.stdout), "hi");
    assert!(run.status.success());
}

#[test]
fn the_cli_component_flag_takes_the_codegen_flags_it_supports() {
    let (_dir, input) = cli_component("compat");
    let code = cli_ok(&[
        "--component".as_ref(),
        "--unsafe-memory".as_ref(),
        "--jobs=2".as_ref(),
        "--split-dispatch=50".as_ref(),
        "--external-types=wasm2rs_wasi".as_ref(),
        input.as_os_str(),
    ]);
    assert!(
        code.contains("get_unchecked"),
        "--unsafe-memory not applied"
    );
}

#[test]
fn the_cli_component_flag_passes_external_types_to_the_glue() {
    let (_dir, input) = cli_component("own");
    let code = cli_ok(&[
        "--component".as_ref(),
        "--external-types=my_host::wasi".as_ref(),
        input.as_os_str(),
    ]);
    assert!(code.contains("use my_host::wasi::Host;"));
}

#[test]
fn the_cli_component_flag_names_the_flag_it_cannot_combine_with() {
    let (dir, input) = cli_component("refuse");
    for flag in [
        "--entry-exports=a",
        "--import-memory",
        "--named-exports",
        "--named-imports",
        "--reentrant-imports",
        "--wit-host",
        "--glue",
        "--dump",
        "--extract-core",
    ] {
        let out = cli(&["--component".as_ref(), flag.as_ref(), input.as_os_str()]);
        assert!(!out.status.success(), "{flag} was accepted");
        let stderr = String::from_utf8_lossy(&out.stderr);
        let name = flag.split('=').next().unwrap();
        assert!(
            stderr.contains("--component") && stderr.contains(name),
            "{flag}: {stderr}"
        );
    }
    // A split needs the `funcs_per_file` positional, which a single source has no use for.
    let out = cli(&[
        "--component".as_ref(),
        input.as_os_str(),
        dir.join("o.rs").as_os_str(),
        "10".as_ref(),
    ]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--component"));
}

#[test]
fn the_cli_component_flag_rejects_a_core_module() {
    let core = wat::parse_str("(module (func (export \"f\")))").unwrap();
    let (_dir, input) = cli_input("core", &core);
    let out = cli(&["--component".as_ref(), input.as_os_str()]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("core module") && stderr.contains("transpile"),
        "{stderr}"
    );
}

// --- an adapter-shaped component (the program module imports its memory from another module) ---

/// The `wasm-tools component new --adapt` shape: the module that lifts the export (the adapter)
/// imports `env.memory` and `main._start` from the user's module.
fn adapter_shaped_component() -> Vec<u8> {
    wat::parse_str(
        r#"(component
            (core module $main (memory (export "memory") 1) (func (export "_start")))
            (core module $adapter
                (import "env" "memory" (memory 1))
                (import "main" "_start" (func $start))
                (func (export "run") call $start))
            (core instance $m (instantiate $main))
            (alias core export $m "memory" (core memory $mem))
            (alias core export $m "_start" (core func $start))
            (core instance $env (export "memory" (memory $mem)))
            (core instance $mainfns (export "_start" (func $start)))
            (core instance $a (instantiate $adapter
                (with "env" (instance $env))
                (with "main" (instance $mainfns))))
            (alias core export $a "run" (core func $run))
            (func (export "run") (canon lift (core func $run))))"#,
    )
    .unwrap()
}

#[test]
fn a_program_module_importing_memory_from_another_core_module_is_rejected_clearly() {
    let err =
        transpile_component(&adapter_shaped_component(), &ComponentOptions::default()).unwrap_err();
    assert!(matches!(err, TranspileError::Unsupported(_)), "{err:?}");
    let msg = err.to_string();
    assert!(msg.contains("another core module"), "{msg}");
    assert!(msg.contains("memory"), "{msg}");
    assert!(!msg.contains("reentrant_imports"), "{msg}");
}
