//! Integration tests for Phase 4b: `call_indirect`, the table section and
//! active element segments. Generated code dispatches through a `match` on the
//! table entry (a function index), so a wrong-type, null or out-of-bounds entry
//! traps (panics). Each test compiles the generated Rust with `rustc -D
//! warnings`; behaviour tests then assert results, and trap tests assert the
//! program panics at run time.

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

/// Compile the module generated from `wat`, returning the binary path. Fails
/// the test if generation or compilation does not succeed.
fn compile(test: &str, wat: &str, main_body: &str) -> std::path::PathBuf {
    let wasm = wat::parse_str(wat).expect("valid wat");
    let generated = wasm2rs::transpile(&wasm).expect("transpile ok");

    let dir = std::env::temp_dir().join(format!("wasm2rs_indirect_{test}"));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let src = dir.join("gen.rs");
    let bin = dir.join(if cfg!(windows) { "gen.exe" } else { "gen" });

    let program = format!("{generated}\nfn main() {{\n{main_body}\n}}\n");
    std::fs::write(&src, &program).expect("write generated source");

    // Run with the per-test dir as the working directory so each concurrent
    // `rustc` writes its codegen-unit temp objects in isolation (otherwise
    // parallel invocations clobber each other's `*.rcgu.o` files in the CWD).
    let out = Command::new("rustc")
        .current_dir(&dir)
        .arg(&src)
        .arg("--edition")
        .arg("2024")
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

/// The generated program must compile and run to completion (its own assertions
/// hold).
fn expect_ok(test: &str, wat: &str, main_body: &str) {
    let bin = compile(test, wat, main_body);
    let run = Command::new(&bin).status().expect("run generated binary");
    assert!(run.success(), "generated program did not succeed: {test}");
}

/// The generated program must compile but trap (panic) at run time.
fn expect_trap(test: &str, wat: &str, main_body: &str) {
    let bin = compile(test, wat, main_body);
    let run = Command::new(&bin).output().expect("run generated binary");
    assert!(
        !run.status.success(),
        "expected a trap but the program exited cleanly: {test}",
    );
}

const BINOP_TABLE: &str = r#"
    (module
      (type $binop (func (param i32 i32) (result i32)))
      (table 2 funcref)
      (elem (i32.const 0) $add $sub)
      (func $add (param i32 i32) (result i32) (i32.add (local.get 0) (local.get 1)))
      (func $sub (param i32 i32) (result i32) (i32.sub (local.get 0) (local.get 1)))
      (func $dispatch (param i32 i32 i32) (result i32)
        (call_indirect (type $binop) (local.get 1) (local.get 2) (local.get 0))))
    "#;

#[test]
fn dispatches_to_selected_function() {
    // dispatch(index, a, b) invokes table[index](a, b): slot 0 = add, 1 = sub.
    expect_ok(
        "dispatch",
        BINOP_TABLE,
        "let mut inst = Instance::new();\n    \
         assert_eq!(inst.func2(0, 10, 3), 13);\n    \
         assert_eq!(inst.func2(1, 10, 3), 7);",
    );
}

#[test]
fn out_of_bounds_index_traps() {
    expect_trap(
        "oob",
        BINOP_TABLE,
        "let mut inst = Instance::new();\n    inst.func2(5, 10, 3);",
    );
}

#[test]
fn null_table_slot_traps() {
    // The table has two slots but the element segment fills only slot 0, so
    // index 1 is a null funcref and calling through it must trap.
    expect_trap(
        "null",
        r#"
        (module
          (type $binop (func (param i32 i32) (result i32)))
          (table 2 funcref)
          (elem (i32.const 0) $add)
          (func $add (param i32 i32) (result i32) (i32.add (local.get 0) (local.get 1)))
          (func $dispatch (param i32 i32 i32) (result i32)
            (call_indirect (type $binop) (local.get 1) (local.get 2) (local.get 0))))
        "#,
        "let mut inst = Instance::new();\n    inst.func1(1, 10, 3);",
    );
}

#[test]
fn type_mismatch_traps() {
    // The table holds a unary function but the indirect call expects a binop
    // type, so the runtime type check must fail (trap).
    expect_trap(
        "mismatch",
        r#"
        (module
          (type $binop (func (param i32 i32) (result i32)))
          (table 1 funcref)
          (elem (i32.const 0) $unary)
          (func $unary (param i32) (result i32) (local.get 0))
          (func $dispatch (param i32 i32) (result i32)
            (call_indirect (type $binop) (local.get 0) (local.get 1) (i32.const 0))))
        "#,
        "let mut inst = Instance::new();\n    inst.func1(10, 3);",
    );
}

#[test]
fn void_indirect_call_runs_for_effect() {
    // A result-less indirect call to a function that stores into memory, then
    // the stored value is read back — exercises table + memory together.
    expect_ok(
        "void",
        r#"
        (module
          (type $storer (func (param i32 i32)))
          (memory 1)
          (table 1 funcref)
          (elem (i32.const 0) $store)
          (func $store (param i32 i32) (i32.store (local.get 0) (local.get 1)))
          (func $run (param i32 i32) (result i32)
            (call_indirect (type $storer) (local.get 0) (local.get 1) (i32.const 0))
            (i32.load (local.get 0))))
        "#,
        "let mut inst = Instance::new();\n    \
         assert_eq!(inst.func1(8, 12345), 12345);",
    );
}
