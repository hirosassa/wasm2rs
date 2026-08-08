//! Integration tests for `externref` values and `externref` tables. An
//! `externref` is an opaque host reference; here it is represented as an opaque
//! `u32` handle (`u32::MAX` is null), the same representation as a `funcref`, so
//! it reuses the existing table machinery (`Vec<u32>`). The module never
//! inspects the handle; it only moves, stores, null-checks and passes it. The
//! generated Rust is compiled with `rustc -D warnings` and exercised.

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

fn compile(test: &str, wat: &str, extra: &str) -> std::path::PathBuf {
    let wasm = wat::parse_str(wat).expect("valid wat");
    let generated = wasm2rs::transpile(&wasm).expect("transpile ok");

    let dir = std::env::temp_dir().join(format!("wasm2rs_externref_{test}"));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let src = dir.join("gen.rs");
    let bin = dir.join(if cfg!(windows) { "gen.exe" } else { "gen" });

    let program = format!("{generated}\n{extra}\n");
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

fn run_main(test: &str, wat: &str, main_body: &str) {
    let extra = format!("fn main() {{\n{main_body}\n}}\n");
    let bin = compile(test, wat, &extra);
    let run = Command::new(&bin).status().expect("run generated binary");
    assert!(
        run.success(),
        "generated program assertions failed:\n{test}"
    );
}

#[test]
fn ref_null_extern_is_null() {
    // `ref.is_null (ref.null extern)` is 1; a non-null externref handle is 0.
    let wat = r#"
        (module
          (func (result i32) (ref.is_null (ref.null extern)))
          (func (param externref) (result i32) (ref.is_null (local.get 0))))
        "#;
    run_main(
        "null_is_null",
        wat,
        "assert_eq!(func0(), 1);\n    \
         assert_eq!(func1(u32::MAX), 1);\n    \
         assert_eq!(func1(42u32), 0);",
    );
}

#[test]
fn externref_round_trips_through_params_and_results() {
    // A function taking and returning an `externref` is the identity on the
    // opaque handle; a null passes through unchanged.
    let wat = r#"
        (module
          (func $id (param externref) (result externref) (local.get 0))
          (func (param externref) (result externref) (call $id (local.get 0))))
        "#;
    run_main(
        "params",
        wat,
        "assert_eq!(func1(7u32), 7u32);\n    \
         assert_eq!(func1(u32::MAX), u32::MAX);",
    );
}

#[test]
fn externref_global_round_trips() {
    // A mutable `externref` global initialised to null; storing a handle and
    // reading it back preserves the value and its null-ness.
    let wat = r#"
        (module
          (global $g (mut externref) (ref.null extern))
          (func $set (param externref) (global.set $g (local.get 0)))
          (func $is_null (result i32) (ref.is_null (global.get $g)))
          (func $get (result externref) (global.get $g)))
        "#;
    run_main(
        "global",
        wat,
        "let mut inst = Instance::new();\n    \
         assert_eq!(inst.func1(), 1);\n    \
         inst.func0(88u32);\n    \
         assert_eq!(inst.func1(), 0);\n    \
         assert_eq!(inst.func2(), 88u32);",
    );
}

// A defined `externref` table plus accessors: set/get/size/grow/fill.
const TABLE_M: &str = r#"
    (module
      (table 3 externref)
      (func $set (param i32 externref) (table.set (local.get 0) (local.get 1)))
      (func $get (param i32) (result externref) (table.get (local.get 0)))
      (func $is_null (param i32) (result i32) (ref.is_null (table.get (local.get 0))))
      (func $size (result i32) (table.size))
      (func $grow (param i32 externref) (result i32) (table.grow (local.get 1) (local.get 0)))
      (func $fill (param i32 externref i32) (table.fill (local.get 0) (local.get 1) (local.get 2))))
    "#;

#[test]
fn externref_table_set_get_size_grow_fill() {
    run_main(
        "table",
        TABLE_M,
        "let mut inst = Instance::new();\n    \
         assert_eq!(inst.func3(), 3);\n    \
         assert_eq!(inst.func2(0), 1);\n    \
         inst.func0(0, 55u32);\n    \
         assert_eq!(inst.func2(0), 0);\n    \
         assert_eq!(inst.func1(0), 55u32);\n    \
         assert_eq!(inst.func4(2, 77u32), 3);\n    \
         assert_eq!(inst.func3(), 5);\n    \
         assert_eq!(inst.func1(4), 77u32);\n    \
         inst.func5(1, 9u32, 2);\n    \
         assert_eq!(inst.func1(1), 9u32);\n    \
         assert_eq!(inst.func1(2), 9u32);",
    );
}

#[test]
fn externref_table_get_out_of_bounds_traps() {
    let extra = "fn main() { let mut inst = Instance::new(); inst.func1(9); }";
    let bin = compile("table_oob", TABLE_M, extra);
    let run = Command::new(&bin).output().expect("run generated binary");
    assert!(
        !run.status.success(),
        "expected a trap indexing past the table"
    );
}

// A host-owned (imported) `externref` table, lent through the `Imports` trait as
// `Vec<u32>` exactly like an imported funcref table (Tier 1).
const IMPORTED_TABLE_M: &str = r#"
    (module
      (import "env" "tbl" (table 2 externref))
      (func $set (param i32 externref) (table.set (local.get 0) (local.get 1)))
      (func $get (param i32) (result externref) (table.get (local.get 0))))
    "#;

const HOST_TABLE: &str = r#"
    struct Host { table: Vec<u32> }
    impl Imports for Host {
        fn table(&self) -> &[u32] { &self.table }
        fn table_mut(&mut self) -> &mut Vec<u32> { &mut self.table }
    }
"#;

#[test]
fn imported_externref_table_round_trips() {
    let extra = format!(
        "{HOST_TABLE}\n\
         fn main() {{\n    \
         let host = Host {{ table: vec![u32::MAX; 2] }};\n    \
         let mut inst = Instance::new(host);\n    \
         inst.func0(1, 123u32);\n    \
         assert_eq!(inst.func1(1), 123u32);\n    \
         assert_eq!(inst.func1(0), u32::MAX);\n    \
         }}"
    );
    let bin = compile("imported", IMPORTED_TABLE_M, &extra);
    let run = Command::new(&bin).status().expect("run generated binary");
    assert!(run.success(), "imported externref table round-trip failed");
}
