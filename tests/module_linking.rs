//! Integration tests for cross-instance linking, Phase L0 (static linking).
//!
//! Two modules are transpiled separately and placed in sibling Rust modules
//! (`mod a`, `mod b`) so their generated `Imports`/`Instance`/`func{n}` items do
//! not collide. Module A imports a function by name; the host wires that import
//! to a call into module B (a free function or another `Instance`). A seeds the
//! imported function's index into its table and reaches it through
//! `call_indirect` — so a call routed through A's table ends up executing B's
//! code. This is the "static linking" flavor of cross-instance funcref, and it
//! needs no transpiler change: an imported function occupies A's funcref index
//! space and dispatches through A's host trait, whose implementation is free to
//! call into B. The generated Rust is compiled with `rustc -D warnings` and run.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::arithmetic_side_effects,
    clippy::float_cmp,
    clippy::lossy_float_literal,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::unwrap_in_result,
    reason = "test code"
)]

use std::process::Command;

/// Transpile each `(module_name, wat)` into `pub mod module_name { … }`, append
/// `extra` (host glue plus `fn main`), then compile under `-D warnings`.
fn compile(test: &str, modules: &[(&str, &str)], extra: &str) -> std::path::PathBuf {
    let mut program = String::new();
    for (name, wat) in modules {
        let wasm = wat::parse_str(wat).expect("valid wat");
        let generated = wasm2rs::transpile(&wasm).expect("transpile ok");
        program.push_str(&format!("pub mod {name} {{\n{generated}\n}}\n"));
    }
    program.push_str(extra);
    program.push('\n');

    let dir = std::env::temp_dir().join(format!("wasm2rs_linking_{test}"));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let src = dir.join("gen.rs");
    let bin = dir.join(if cfg!(windows) { "gen.exe" } else { "gen" });
    std::fs::write(&src, &program).expect("write generated source");

    let out = Command::new("rustc")
        .current_dir(&dir)
        .arg(&src)
        .arg("--edition")
        .arg("2021")
        .arg("-D")
        .arg("warnings")
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("run rustc");
    assert!(
        out.status.success(),
        "generated code failed to compile:\n{program}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&out.stderr),
    );
    bin
}

fn run_ok(test: &str, modules: &[(&str, &str)], extra: &str) {
    let bin = compile(test, modules, extra);
    let run = Command::new(&bin).status().expect("run generated binary");
    assert!(
        run.success(),
        "generated program assertions failed:\n{test}"
    );
}

#[test]
fn call_indirect_reaches_a_stateless_other_module() {
    // B exports a pure `square`; with no state it becomes a free `func0`.
    let b = r#"
        (module
          (func (export "square") (param i32) (result i32)
            (i32.mul (local.get 0) (local.get 0))))
        "#;
    // A imports B's `square`, seeds it into its table via an element segment,
    // and dispatches through `call_indirect`.
    let a = r#"
        (module
          (type $sig (func (param i32) (result i32)))
          (import "b" "square" (func $bsquare (type $sig)))
          (table 1 funcref)
          (elem (i32.const 0) $bsquare)
          (func $call (param i32) (result i32)
            (call_indirect (type $sig) (local.get 0) (i32.const 0))))
        "#;
    let extra = r#"
        struct AHost;
        impl a::Imports for AHost {
            fn import0(&mut self, a0: i32) -> i32 { b::func0(a0) }
        }
        fn main() {
            let mut inst = a::Instance::new(AHost);
            assert_eq!(inst.func1(7), 49);
        }
    "#;
    run_ok("stateless", &[("a", a), ("b", b)], extra);
}

#[test]
fn call_indirect_drives_state_in_another_instance() {
    // B keeps a mutable counter, so it becomes an `Instance` the host owns.
    let b = r#"
        (module
          (global $c (mut i32) (i32.const 0))
          (func (export "next") (result i32)
            (global.set $c (i32.add (global.get $c) (i32.const 1)))
            (global.get $c)))
        "#;
    // A imports B's `next` and reaches it through its table; each indirect call
    // advances B's counter, proving genuine cross-instance state.
    let a = r#"
        (module
          (type $sig (func (result i32)))
          (import "b" "next" (func $bnext (type $sig)))
          (table 1 funcref)
          (elem (i32.const 0) $bnext)
          (func $tick (result i32) (call_indirect (type $sig) (i32.const 0))))
        "#;
    let extra = r#"
        struct AHost { b: b::Instance }
        impl a::Imports for AHost {
            fn import0(&mut self) -> i32 { self.b.func0() }
        }
        fn main() {
            let mut inst = a::Instance::new(AHost { b: b::Instance::new() });
            assert_eq!(inst.func1(), 1);
            assert_eq!(inst.func1(), 2);
            assert_eq!(inst.func1(), 3);
        }
    "#;
    run_ok("stateful", &[("a", a), ("b", b)], extra);
}
