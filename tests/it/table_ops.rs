//! Integration tests for Phase 8 (table instructions): `table.get`,
//! `table.set`, `table.size`, `table.grow` and the previously deferred
//! `table.fill`. A table entry and a `funcref` operand are both `u32` function
//! indices (`u32::MAX` is null). Each module declares a table, so it becomes a
//! `struct Instance`; the generated Rust is compiled with `rustc -D warnings`
//! and exercised. Traps are verified by the generated binary exiting
//! unsuccessfully.

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

fn compile(test: &str, wat: &str, main_body: &str) -> std::path::PathBuf {
    let wasm = wat::parse_str(wat).expect("valid wat");
    let generated = wasm2rs::transpile(&wasm).expect("transpile ok");

    let dir = std::env::temp_dir().join(format!("wasm2rs_table_ops_{test}"));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let src = dir.join("gen.rs");
    let bin = dir.join(if cfg!(windows) { "gen.exe" } else { "gen" });

    let program = format!("{generated}\nfn main() {{\n{main_body}\n}}\n");
    std::fs::write(&src, &program).expect("write generated source");

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

fn transpile_compile_run(test: &str, wat: &str, main_body: &str) {
    let bin = compile(test, wat, main_body);
    let run = Command::new(&bin).status().expect("run generated binary");
    assert!(
        run.success(),
        "generated program assertions failed:\n{test}"
    );
}

fn expect_trap(test: &str, wat: &str, main_body: &str) {
    let bin = compile(test, wat, main_body);
    let run = Command::new(&bin).output().expect("run generated binary");
    assert!(!run.status.success(), "expected a trap:\n{test}");
}

// `table.set`/`table.get` a funcref, observe it via `ref.is_null`, and dispatch
// through it with `call_indirect`.
const GET_SET_M: &str = r#"
    (module
      (type $sig (func (result i32)))
      (table 2 funcref)
      (func $a (type $sig) (i32.const 99))
      (func (param i32) (table.set (local.get 0) (ref.func $a)))
      (func (param i32) (result i32) (ref.is_null (table.get (local.get 0))))
      (func (param i32) (result i32) (call_indirect (type $sig) (local.get 0))))
    "#;

#[test]
fn table_set_then_get_and_call() {
    transpile_compile_run(
        "get_set",
        GET_SET_M,
        "let mut inst = Instance::new();\n    \
         assert_eq!(inst.func2(0), 1);\n    \
         inst.func1(0);\n    \
         assert_eq!(inst.func2(0), 0);\n    \
         assert_eq!(inst.func3(0), 99);",
    );
}

#[test]
fn table_get_out_of_bounds_traps() {
    expect_trap(
        "get_oob",
        GET_SET_M,
        "let mut inst = Instance::new(); inst.func2(5);",
    );
}

#[test]
fn table_set_out_of_bounds_traps() {
    expect_trap(
        "set_oob",
        GET_SET_M,
        "let mut inst = Instance::new(); inst.func1(5);",
    );
}

// `table.size` reports the current length; `table.grow` appends null slots and
// returns the old length.
const SIZE_GROW_M: &str = r#"
    (module
      (table 3 funcref)
      (func (result i32) (table.size))
      (func (param i32) (result i32) (table.grow (ref.null func) (local.get 0))))
    "#;

#[test]
fn table_size_and_grow() {
    transpile_compile_run(
        "size_grow",
        SIZE_GROW_M,
        "let mut inst = Instance::new();\n    \
         assert_eq!(inst.func0(), 3);\n    \
         assert_eq!(inst.func1(2), 3);\n    \
         assert_eq!(inst.func0(), 5);",
    );
}

// `table.fill` writes a funcref into a range; dispatch confirms the entries.
const FILL_M: &str = r#"
    (module
      (type $sig (func (result i32)))
      (table 4 funcref)
      (func $a (type $sig) (i32.const 7))
      (func (param i32 i32) (table.fill (local.get 0) (ref.func $a) (local.get 1)))
      (func (param i32) (result i32) (call_indirect (type $sig) (local.get 0))))
    "#;

#[test]
fn table_grow_returns_minus_one_when_unsatisfiable() {
    // A "negative" delta is a huge unsigned count; growth cannot be satisfied,
    // so `table.grow` must push -1 (and not abort) and leave the size unchanged.
    transpile_compile_run(
        "grow_fail",
        SIZE_GROW_M,
        "let mut inst = Instance::new();\n    \
         assert_eq!(inst.func1(-1), -1);\n    \
         assert_eq!(inst.func0(), 3);",
    );
}

#[test]
fn table_fill_writes_a_range() {
    transpile_compile_run(
        "fill",
        FILL_M,
        "let mut inst = Instance::new();\n    \
         inst.func1(1, 2);\n    \
         assert_eq!(inst.func2(1), 7);\n    \
         assert_eq!(inst.func2(2), 7);",
    );
}

#[test]
fn table_fill_out_of_bounds_traps() {
    // dest=3, len=2 writes slot 4, past the 4-entry table (indices 0..=3).
    expect_trap(
        "fill_oob",
        FILL_M,
        "let mut inst = Instance::new(); inst.func1(3, 2);",
    );
}
