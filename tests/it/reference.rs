//! Integration tests for Phase 9 (reference types): `ref.null func`,
//! `ref.func` and `ref.is_null`. A `funcref` value is represented as a `u32`
//! function index on the operand stack (`u32::MAX` is a null funcref), matching
//! the table's element representation. Modules here declare no memory/table/
//! global/import, so they transpile to free `func{n}` functions; the generated
//! Rust is compiled with `rustc -D warnings` and exercised.

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

    let dir = std::env::temp_dir().join(format!("wasm2rs_reference_{test}"));
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

#[test]
fn ref_is_null_distinguishes_null_from_a_function() {
    // `ref.is_null (ref.null func)` is 1; `ref.is_null (ref.func $target)` is 0.
    let wat = r#"
        (module
          (func $target (result i32) (i32.const 42))
          (func (result i32) (ref.is_null (ref.null func)))
          (func (result i32) (ref.is_null (ref.func $target))))
        "#;
    transpile_compile_run(
        "ref_is_null",
        wat,
        "assert_eq!(func1(), 1);\n    assert_eq!(func2(), 0);",
    );
}

#[test]
fn funcref_round_trips_through_a_local() {
    // A `funcref` local is a `u32`; storing `ref.func` and reading it back keeps
    // it non-null.
    let wat = r#"
        (module
          (func $target (result i32) (i32.const 42))
          (func (result i32) (local funcref)
            (local.set 0 (ref.func $target))
            (ref.is_null (local.get 0))))
        "#;
    transpile_compile_run("funcref_local", wat, "assert_eq!(func1(), 0);");
}

#[test]
fn funcref_passes_through_params_and_results() {
    // A function taking and returning a `funcref` is the identity on the `u32`;
    // passing a null through it stays null.
    let wat = r#"
        (module
          (func $id (param funcref) (result funcref) (local.get 0))
          (func (result i32) (ref.is_null (call $id (ref.null func)))))
        "#;
    transpile_compile_run("funcref_param", wat, "assert_eq!(func1(), 1);");
}

#[test]
fn declared_element_segment_allows_ref_func() {
    // A declared element segment has no runtime effect; it only permits
    // `ref.func`, so the module must still transpile and run.
    let wat = r#"
        (module
          (func $target (result i32) (i32.const 7))
          (elem declare func $target)
          (func (result i32) (ref.is_null (ref.func $target))))
        "#;
    transpile_compile_run("declared_elem", wat, "assert_eq!(func1(), 0);");
}

#[test]
fn declared_before_passive_keeps_table_init_index_aligned() {
    // A declared segment sits at element index 0 and a passive one at index 1.
    // `table.init 1` must reference the passive segment, so the declared slot
    // must not shift the passive segment's index.
    let wat = r#"
        (module
          (type $sig (func (result i32)))
          (table 2 funcref)
          (func $a (type $sig) (i32.const 55))
          (elem declare func $a)
          (elem func $a)
          (func (param i32 i32 i32) (table.init 1 (local.get 0) (local.get 1) (local.get 2)))
          (func (param i32) (result i32) (call_indirect (type $sig) (local.get 0))))
        "#;
    transpile_compile_run(
        "declared_before_passive",
        wat,
        "let mut inst = Instance::new();\n    \
         inst.func1(0, 0, 1);\n    \
         assert_eq!(inst.func2(0), 55);",
    );
}
