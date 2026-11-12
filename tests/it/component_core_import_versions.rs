//! The program module may import a WIT function under another version of its interface than
//! the component's own import (`@0.2.0` in the module, `@0.2.12` at the component level), and
//! may import the same function under several of them. The glue must implement exactly the
//! methods of the module's `Imports`, each delegating to the one `Host` method.

use std::process::Command;

use wasm2rs::{ComponentOptions, transpile_component};

use crate::common;

/// A component importing `t:t/i@0.2.12` `f` once, lowered into a core instance per module
/// import name; the program module calls `f` through each of `core_imports` (version names)
/// and exports `run`, the sum of the results.
fn component(core_imports: &[&str]) -> Vec<u8> {
    let mut imports = String::new();
    let mut calls = String::new();
    let mut withs = String::new();
    for (i, version) in core_imports.iter().enumerate() {
        imports.push_str(&format!(
            "(import \"t:t/i@{version}\" \"f\" (func $f{i} (param i32) (result i32)))\n"
        ));
        calls.push_str(&format!("(i32.add (call $f{i} (i32.const 10)))\n"));
        withs.push_str(&format!("(with \"t:t/i@{version}\" (instance $env{i}))\n"));
    }
    let lowered: String = (0..core_imports.len())
        .map(|i| {
            format!(
                "(core func $l{i} (canon lower (func $f)))\n\
                 (core instance $env{i} (export \"f\" (func $l{i})))\n"
            )
        })
        .collect();
    wat::parse_str(format!(
        r#"(component
  (import "t:t/i@0.2.12" (instance $i (export "f" (func (param "n" u32) (result u32)))))
  (alias export $i "f" (func $f))
  (core module $m
    {imports}
    (func (export "run") (result i32)
      (i32.const 0)
      {calls}))
  {lowered}
  (core instance (instantiate $m {withs})))"#
    ))
    .unwrap()
}

/// Builds the transpiled `component` with a host whose `f` adds 1, runs `run` and returns the
/// exit status: 0 when `run` returned `expected`.
fn run_sum(name: &str, component: &[u8], expected: i32) {
    let options = ComponentOptions {
        external_types: None,
        exports: false,
        ..ComponentOptions::default()
    };
    let code = transpile_component(component, &options).unwrap();
    let program = format!(
        "{code}\n\
         struct H;\n\
         impl Host for H {{\n    fn t_t_i_f(&mut self, n: u32) -> u32 {{ n + 1 }}\n}}\n\
         fn main() {{\n    let mut inst = Instance::new(Glue(H));\n    \
         std::process::exit(if inst.run() == {expected} {{ 0 }} else {{ 1 }});\n}}\n"
    );
    let bin = common::build(&format!("component_core_import_versions_{name}"), &program);
    let out = Command::new(bin).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_function_imported_under_another_version_than_the_component_s_is_implemented() {
    run_sum("renamed", &component(&["0.2.0"]), 11);
}

#[test]
fn a_function_the_module_imports_under_two_versions_delegates_to_one_host_method() {
    run_sum("twice", &component(&["0.2.12", "0.2.0"]), 22);
}

#[test]
fn a_module_version_two_component_imports_both_satisfy_is_refused_as_ambiguous() {
    let wasm = wat::parse_str(
        r#"(component
  (import "t:t/i@0.2.12" (instance $a (export "f" (func))))
  (import "t:t/i@0.2.10" (instance $b (export "f" (func))))
  (alias export $a "f" (func $f))
  (core module $m (import "t:t/i@0.2.0" "f" (func)) (func (export "run") call 0))
  (core func $l (canon lower (func $f)))
  (core instance $env (export "f" (func $l)))
  (core instance (instantiate $m (with "t:t/i@0.2.0" (instance $env)))))"#,
    )
    .unwrap();
    let err = transpile_component(&wasm, &ComponentOptions::default())
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("t:t/i@0.2.0") && err.contains("several compatible versions"),
        "{err}"
    );
}

#[test]
fn a_pre_release_or_zero_zero_version_is_not_compatible_with_another() {
    for module in ["t:t/i@0.2.0-rc-1", "t:t/i@0.0.1", "t:t/i@1.0.0"] {
        let err = transpile_component(
            &component(&[module.trim_start_matches("t:t/i@")]),
            &ComponentOptions::default(),
        );
        // Nothing the component imports (`@0.2.12`) stands for it: the glue leaves it out.
        let code = err.unwrap();
        assert!(!code.contains("self.0.t_t_i_f("), "{module}");
    }
}

#[test]
fn a_major_version_is_compatible_across_minors() {
    let wasm = wat::parse_str(
        r#"(component
  (import "t:t/i@1.3.1" (instance $i (export "f" (func))))
  (alias export $i "f" (func $f))
  (core module $m (import "t:t/i@1.2.0" "f" (func)) (func (export "run") call 0))
  (core func $l (canon lower (func $f)))
  (core instance $env (export "f" (func $l)))
  (core instance (instantiate $m (with "t:t/i@1.2.0" (instance $env)))))"#,
    )
    .unwrap();
    let code = transpile_component(&wasm, &ComponentOptions::default()).unwrap();
    assert!(code.contains("self.0.t_t_i_f("));
}
