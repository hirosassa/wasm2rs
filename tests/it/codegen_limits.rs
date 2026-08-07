//! Tests that the code generator bounds the textual size of a single generated
//! expression. Without a cap, a long straight-line chain of "stable" operations
//! (constants, immutable locals) folds into one enormous Rust expression on a
//! single line — real binaries produced multi-megabyte lines that rustc cannot
//! parse. The generator must spill such a chain into intermediate `let`
//! bindings while preserving the computed value.

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

/// A module whose single function adds `1` to a running total `k` times. Each
/// `i32.const 1 i32.add` extends a stable expression, so an uncapped generator
/// emits it all on one line.
fn long_add_chain(k: usize) -> String {
    let mut body = String::from("        i32.const 0\n");
    for _ in 0..k {
        body.push_str("        i32.const 1 i32.add\n");
    }
    format!("(module (func (export \"f\") (result i32)\n{body}    ))")
}

#[test]
fn long_expression_chains_are_materialized_into_bounded_lines() {
    let k = 5000;
    let wat = long_add_chain(k);
    let wasm = wat::parse_str(&wat).expect("valid wat");
    let src = wasm2rs::transpile(&wasm).expect("transpile ok");

    let longest = src.lines().map(str::len).max().unwrap_or(0);
    assert!(
        longest <= 8192,
        "a generated line is {longest} bytes long; the expression-size cap was not applied",
    );

    // The spilling must not change the result: 0 + 1*k == k.
    compile_run("limits_chain", &wat, &format!("assert_eq!(func0(), {k});"));
}

#[test]
fn large_data_segment_is_wrapped_across_lines() {
    // A big active data segment must not render as one giant byte-array line.
    // 4000 bytes of 0x41 ('A') initialised at offset 0; check byte 3999 too so
    // the whole segment is verified to have been copied.
    let payload = "A".repeat(4000);
    let wat = format!(
        r#"(module
             (memory 1)
             (data (i32.const 0) "{payload}")
             (func (export "at") (param i32) (result i32) (i32.load8_u (local.get 0))))"#
    );
    let wasm = wat::parse_str(&wat).expect("valid wat");
    let src = wasm2rs::transpile(&wasm).expect("transpile ok");

    let longest = src.lines().map(str::len).max().unwrap_or(0);
    assert!(
        longest <= 8192,
        "a generated line is {longest} bytes long; the data literal was not wrapped",
    );

    compile_run(
        "limits_data",
        &wat,
        "let mut inst = Instance::new();\n    \
         assert_eq!(inst.func0(0), 0x41);\n    \
         assert_eq!(inst.func0(3999), 0x41);",
    );
}
