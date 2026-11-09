//! A WebAssembly *component* is rejected with a clear error instead of being
//! mis-parsed as a core module.

use std::process::Command;

use wasm2rs::{
    ComponentOptions, TranspileError, TranspileOptions, dump, transpile, transpile_component,
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
fn unsupported(result: Result<String, TranspileError>) -> String {
    match result {
        Err(TranspileError::Unsupported(why)) => why,
        other => panic!("expected Unsupported, got {other:?}"),
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
fn the_generated_source_builds_and_runs_the_typed_run_export() {
    let code =
        transpile_component(&component_of(WORLD, CORE), &ComponentOptions::default()).unwrap();
    let program = format!(
        "{code}\nfn main() {{\n    \
         let host = wasm2rs_wasi::WasiHost::new(Vec::new()).with_arguments(vec![\"p\".to_string(), \"hello\".to_string()]);\n    \
         let mut inst = Instance::new(Glue(host));\n    \
         let result = Exports::export_wasi_cli_run_0_2_0_run(&mut inst);\n    \
         drop(inst);\n    \
         std::process::exit(if result.is_ok() {{ 0 }} else {{ 1 }});\n}}\n"
    );
    let bin = common::build_with_wasi("component_input_run", &program);
    let out = Command::new(bin).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "hello");
    assert!(out.status.success());
}

#[test]
fn the_options_the_glue_needs_are_forced_on() {
    let options = ComponentOptions {
        transpile: TranspileOptions {
            named_imports: false,
            named_exports: false,
            reentrant_imports: false,
            ..TranspileOptions::default()
        },
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
    let why = unsupported(transpile_component(
        &component,
        &ComponentOptions::default(),
    ));
    assert!(why.contains("more than one core module"), "{why}");
}

#[test]
fn a_component_without_a_core_module_is_unsupported() {
    let component = wat_component("(component)");
    let why = unsupported(transpile_component(
        &component,
        &ComponentOptions::default(),
    ));
    assert!(why.contains("no core module"), "{why}");
}

#[test]
fn a_core_module_input_says_to_use_transpile() {
    let wasm = wat::parse_str("(module (func (export \"f\")))").unwrap();
    let why = unsupported(transpile_component(&wasm, &ComponentOptions::default()));
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
