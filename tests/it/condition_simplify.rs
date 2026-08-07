//! Tests that a comparison feeding a branch condition is used directly as a
//! Rust `bool` instead of being wrapped as `i32::from(cmp) != 0`.
//!
//! A wasm comparison yields an i32 (0 or 1), so the generator renders it as
//! `i32::from(a == b)`; `if`/`br_if`/`select` then test that i32 with `!= 0`.
//! In a condition the whole `i32::from(cmp) != 0` collapses to just `cmp`. A
//! condition that is *not* a comparison (a raw i32 value) must keep `!= 0`.
//! Behaviour must stay identical, so each shape check is paired with a run.

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

use crate::common;

use common::compile_run;

fn transpile(wat: &str) -> String {
    let wasm = wat::parse_str(wat).expect("valid wat");
    wasm2rs::transpile(&wasm).expect("transpile ok")
}

#[test]
fn if_condition_uses_the_comparison_directly() {
    let wat = r#"(module (func (export "f") (param i32 i32) (result i32)
        (if (result i32) (i32.eq (local.get 0) (local.get 1))
            (then (i32.const 10)) (else (i32.const 20)))))"#;
    let src = transpile(wat);

    assert!(
        src.contains("if l0 == l1 {"),
        "expected the `if` to test the comparison directly\n{src}",
    );
    assert!(
        !src.contains("i32::from"),
        "the comparison should not be wrapped for a condition\n{src}",
    );

    compile_run(
        "cond_if",
        wat,
        "assert_eq!(func0(5, 5), 10);\n    assert_eq!(func0(5, 6), 20);",
    );
}

#[test]
fn br_if_condition_uses_the_comparison_directly() {
    let wat = r#"(module (func (export "f") (param i32) (result i32)
        (block
            (br_if 0 (i32.eqz (local.get 0)))
            (return (i32.const 1)))
        (i32.const 0)))"#;
    let src = transpile(wat);

    assert!(
        src.contains("if l0 == 0 {"),
        "expected the `br_if` to test the comparison directly\n{src}",
    );
    assert!(
        !src.contains("i32::from"),
        "the comparison should not be wrapped for a condition\n{src}",
    );

    compile_run(
        "cond_br_if",
        wat,
        "assert_eq!(func0(0), 0);\n    assert_eq!(func0(5), 1);",
    );
}

#[test]
fn select_condition_uses_the_comparison_directly() {
    let wat = r#"(module (func (export "f") (param i32 i32) (result i32)
        (select (local.get 0) (local.get 1) (i32.eqz (local.get 0)))))"#;
    let src = transpile(wat);

    assert!(
        src.contains("if l0 == 0 {"),
        "expected the `select` to test the comparison directly\n{src}",
    );
    assert!(
        !src.contains("i32::from"),
        "the comparison should not be wrapped for a condition\n{src}",
    );

    compile_run(
        "cond_select",
        wat,
        "assert_eq!(func0(0, 9), 0);\n    assert_eq!(func0(5, 9), 9);",
    );
}

#[test]
fn non_comparison_condition_keeps_the_zero_test() {
    // A raw i32 value (not a comparison) has no `i32::from` wrapper, so the
    // condition must still explicitly test `!= 0`.
    let wat = r#"(module (func (export "f") (param i32) (result i32)
        (if (result i32) (local.get 0)
            (then (i32.const 1)) (else (i32.const 2)))))"#;
    let src = transpile(wat);

    assert!(
        src.contains("if l0 != 0 {"),
        "a non-comparison condition must keep the explicit `!= 0`\n{src}",
    );

    compile_run(
        "cond_raw",
        wat,
        "assert_eq!(func0(0), 2);\n    assert_eq!(func0(7), 1);",
    );
}
