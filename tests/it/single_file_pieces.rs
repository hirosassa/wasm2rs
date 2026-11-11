//! `transpile_pieces_with_options` must hand back the same bytes as
//! `transpile_with_options`, however the module is shaped (stateful or not).

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use wasm2rs::{TranspileOptions, transpile_pieces_with_options, transpile_with_options};

fn assert_pieces_match(wat: &str) {
    let wasm = wat::parse_str(wat).expect("valid wat");
    let opts = TranspileOptions::default();
    let joined = transpile_with_options(&wasm, &opts).expect("transpile ok");
    let pieces = transpile_pieces_with_options(&wasm, &opts).expect("pieces ok");

    let mut written = Vec::new();
    pieces.write_to(&mut written).expect("write to Vec");
    assert_eq!(written, joined.as_bytes());
    assert_eq!(pieces.into_string(), joined);
}

#[test]
fn stateful_module_pieces_are_byte_identical() {
    assert_pieces_match(
        r#"(module
            (memory 1)
            (func (export "a") (param i32) (result i32) (i32.load (local.get 0)))
            (func (export "b") (param i32 i32) (i32.store (local.get 0) (local.get 1))))"#,
    );
}

#[test]
fn stateful_module_keeps_bodies_out_of_head_and_tail() {
    let wasm = wat::parse_str(
        r#"(module (memory 1) (func (export "a") (result i32) (i32.load (i32.const 0))))"#,
    )
    .expect("valid wat");
    let pieces = transpile_pieces_with_options(&wasm, &TranspileOptions::default()).unwrap();
    assert!(pieces.body.contains("fn "));
    assert!(!pieces.head.contains("i32.load") && !pieces.head.is_empty());
}

#[test]
fn stateless_module_pieces_are_byte_identical() {
    assert_pieces_match(
        r#"(module
            (func (export "add") (param i32 i32) (result i32)
              (i32.add (local.get 0) (local.get 1))))"#,
    );
}
