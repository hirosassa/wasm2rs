//! Behavioural tests for non-`i32` global *initializers*.
//!
//! `const_expr_to_rust` lowers each global's constant init expression to a Rust
//! literal. i32 initializers are covered elsewhere; here we pin every other
//! constant form — i64, f32, f64, and a `ref.func` funcref — by transpiling a
//! module whose exported getter returns the global, compiling it with
//! `rustc -D warnings`, and asserting the getter yields the exact initial value.
//! A bug that mangled the literal (wrong type suffix, truncated bits, swapped
//! index) would change the observed value and fail here.

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

fn transpile_compile_run(test: &str, wat: &str, main_body: &str) {
    let wasm = wat::parse_str(wat).expect("valid wat");
    let generated = wasm2rs::transpile(&wasm).expect("transpile ok");

    let dir = std::env::temp_dir().join(format!("wasm2rs_global_init_{test}"));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let src = dir.join("gen.rs");
    let bin = dir.join(if cfg!(windows) { "gen.exe" } else { "gen" });

    let program = format!("{generated}\nfn main() {{\n{main_body}\n}}\n");
    std::fs::write(&src, &program).expect("write generated source");

    let compile = Command::new("rustc")
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
        compile.status.success(),
        "generated code failed to compile:\n{program}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&compile.stderr),
    );

    let run = Command::new(&bin).status().expect("run generated binary");
    assert!(
        run.success(),
        "generated program assertions failed:\n{program}"
    );
}

#[test]
fn i64_global_initializer_keeps_its_value() {
    transpile_compile_run(
        "i64",
        r#"
        (module
          (global i64 (i64.const 42))
          (func (export "g") (result i64) (global.get 0)))
        "#,
        "let mut inst = Instance::new();\n    \
         assert_eq!(inst.func0(), 42i64);",
    );
}

#[test]
fn f32_global_initializer_keeps_its_bits() {
    // 1.5 is exactly representable; a bit-level mistake would still surface as a
    // different f32.
    transpile_compile_run(
        "f32",
        r#"
        (module
          (global f32 (f32.const 1.5))
          (func (export "g") (result f32) (global.get 0)))
        "#,
        "let mut inst = Instance::new();\n    \
         assert_eq!(inst.func0(), 1.5f32);",
    );
}

#[test]
fn f64_global_initializer_keeps_its_bits() {
    transpile_compile_run(
        "f64",
        r#"
        (module
          (global f64 (f64.const 2.5))
          (func (export "g") (result f64) (global.get 0)))
        "#,
        "let mut inst = Instance::new();\n    \
         assert_eq!(inst.func0(), 2.5f64);",
    );
}

#[test]
fn funcref_global_initializer_holds_the_function_index() {
    // A `ref.func $f` initializer becomes the function's index in the transpiler's
    // `u32` funcref representation. `$f` is function 0, the getter function 1.
    transpile_compile_run(
        "funcref",
        r#"
        (module
          (func $f (result i32) (i32.const 7))
          (global funcref (ref.func $f))
          (func (export "g") (result funcref) (global.get 0)))
        "#,
        "let mut inst = Instance::new();\n    \
         assert_eq!(inst.func1(), 0u32);",
    );
}

#[test]
fn null_funcref_global_initializer_is_the_null_sentinel() {
    // A `ref.null func` initializer lowers through the `RefNull` arm of
    // `const_expr_to_rust` to the null-funcref sentinel `u32::MAX` — a distinct
    // code path from `ref.func` (index) tested above and from `ref.null` in a
    // function body (tests/reference.rs), which never touches the const-expr
    // lowering. The getter (function 0) returns the raw funcref, and the
    // `ref.is_null` probe (function 1) must see it as null. A regression that
    // lowered null to `0` (a valid function index) would make `func1` return 0.
    transpile_compile_run(
        "null_funcref",
        r#"
        (module
          (global funcref (ref.null func))
          (func (export "g") (result funcref) (global.get 0))
          (func (export "n") (result i32) (ref.is_null (global.get 0))))
        "#,
        "let mut inst = Instance::new();\n    \
         assert_eq!(inst.func0(), u32::MAX);\n    \
         assert_eq!(inst.func1(), 1i32);",
    );
}

#[test]
fn an_extended_const_i32_add_sub_mul_global_is_evaluated() {
    transpile_compile_run(
        "ext_i32",
        r#"
        (module
          (global i32 (i32.add (i32.const 3) (i32.const 4)))
          (global i32 (i32.sub (i32.const 3) (i32.const 10)))
          (global i32 (i32.mul (i32.const 6) (i32.const 7)))
          (func (export "a") (result i32) (global.get 0))
          (func (export "b") (result i32) (global.get 1))
          (func (export "c") (result i32) (global.get 2)))
        "#,
        "let mut inst = Instance::new();\n    \
         assert_eq!(inst.func0(), 7);\n    \
         assert_eq!(inst.func1(), -7);\n    \
         assert_eq!(inst.func2(), 42);",
    );
}

#[test]
fn an_extended_const_i32_global_wraps_on_overflow() {
    transpile_compile_run(
        "ext_i32_wrap",
        r#"
        (module
          (global i32 (i32.add (i32.const 2147483647) (i32.const 1)))
          (global i32 (i32.mul (i32.const 65536) (i32.const 65536)))
          (func (export "a") (result i32) (global.get 0))
          (func (export "b") (result i32) (global.get 1)))
        "#,
        "let mut inst = Instance::new();\n    \
         assert_eq!(inst.func0(), i32::MIN);\n    \
         assert_eq!(inst.func1(), 0);",
    );
}

#[test]
fn an_extended_const_i64_global_is_evaluated_and_wraps() {
    transpile_compile_run(
        "ext_i64",
        r#"
        (module
          (global i64 (i64.mul (i64.add (i64.const 1) (i64.const 2)) (i64.const 4)))
          (global i64 (i64.add (i64.const 9223372036854775807) (i64.const 1)))
          (global i64 (i64.sub (i64.const 0) (i64.const 1)))
          (func (export "a") (result i64) (global.get 0))
          (func (export "b") (result i64) (global.get 1))
          (func (export "c") (result i64) (global.get 2)))
        "#,
        "let mut inst = Instance::new();\n    \
         assert_eq!(inst.func0(), 12i64);\n    \
         assert_eq!(inst.func1(), i64::MIN);\n    \
         assert_eq!(inst.func2(), -1i64);",
    );
}

#[test]
fn a_nested_extended_const_global_is_evaluated() {
    transpile_compile_run(
        "ext_nested",
        r#"
        (module
          (global i32 (i32.mul (i32.add (i32.const 1) (i32.const 2)) (i32.const 4)))
          (func (export "a") (result i32) (global.get 0)))
        "#,
        "let mut inst = Instance::new();\n    \
         assert_eq!(inst.func0(), 12);",
    );
}

#[test]
fn an_extended_const_data_offset_places_the_segment() {
    transpile_compile_run(
        "ext_data_offset",
        r#"
        (module
          (memory 1)
          (data (i32.add (i32.const 8) (i32.const 8)) "\2a")
          (func (export "at") (param i32) (result i32) (i32.load8_u (local.get 0))))
        "#,
        "let mut inst = Instance::new();\n    \
         assert_eq!(inst.func0(16), 42);\n    \
         assert_eq!(inst.func0(8), 0);",
    );
}

#[test]
fn an_extended_const_i64_data_offset_places_the_segment() {
    transpile_compile_run(
        "ext_data_offset64",
        r#"
        (module
          (memory i64 1)
          (data (i64.mul (i64.const 4) (i64.const 5)) "\2a")
          (func (export "at") (param i64) (result i32) (i32.load8_u (local.get 0))))
        "#,
        "let mut inst = Instance::new();\n    \
         assert_eq!(inst.func0(20), 42);",
    );
}

#[test]
fn an_extended_const_elem_offset_places_the_function() {
    transpile_compile_run(
        "ext_elem_offset",
        r#"
        (module
          (type $t (func (result i32)))
          (table 8 funcref)
          (elem (i32.add (i32.const 2) (i32.const 3)) $f)
          (func $f (result i32) (i32.const 77))
          (func (export "call") (param i32) (result i32)
            (call_indirect (type $t) (local.get 0))))
        "#,
        "let mut inst = Instance::new();\n    \
         assert_eq!(inst.func1(5), 77);",
    );
}
