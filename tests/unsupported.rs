//! Contract tests for the transpiler's *rejection* behaviour.
//!
//! `transpile` must reject wasm modules that parse cleanly but use a feature
//! wasm2rs does not support yet, and it must do so with a specific
//! `TranspileError::Unsupported` message rather than silently mistranspiling.
//! These modules are all valid wasm (the `wat` crate assembles them); the error
//! therefore comes from wasm2rs' own validation, not from `wasmparser`.
//!
//! Each test asserts the error *category and message*, not merely `is_err()`, so
//! that a regression which starts accepting an unsupported feature — or reports
//! the wrong reason — fails here.

use wasm2rs::{TranspileError, transpile};

/// Assemble `wat`, transpile it, and assert it is rejected as `Unsupported`
/// with a message containing `needle`.
fn assert_unsupported(wat: &str, needle: &str) {
    let wasm = wat::parse_str(wat).expect("wat should assemble to valid wasm");
    match transpile(&wasm) {
        Err(TranspileError::Unsupported(msg)) => assert!(
            msg.contains(needle),
            "wrong rejection reason: expected to contain {needle:?}, got {msg:?}",
        ),
        other => panic!("expected Unsupported({needle:?}), got {other:?}"),
    }
}

#[test]
fn multiple_defined_memories_are_rejected() {
    assert_unsupported(r#"(module (memory 1) (memory 1))"#, "multiple memories");
}

#[test]
fn shared_defined_memory_is_rejected() {
    // A shared memory (threads proposal) parses but is not supported.
    assert_unsupported(r#"(module (memory 1 1 shared))"#, "64-bit or shared memory");
}

#[test]
fn shared_imported_memory_is_rejected() {
    // Same rule on the import path (classify_import), a distinct branch.
    assert_unsupported(
        r#"(module (import "e" "m" (memory 1 1 shared)))"#,
        "64-bit or shared memory",
    );
}

#[test]
fn multiple_tables_are_rejected() {
    assert_unsupported(
        r#"(module (table 1 funcref) (table 1 funcref))"#,
        "multiple tables",
    );
}

#[test]
fn table_with_initializer_expression_is_rejected() {
    // Only a null-initialised table is supported; an explicit init expr is not.
    assert_unsupported(
        r#"(module (func $f) (table 1 1 funcref (ref.func $f)))"#,
        "table with an initializer expression",
    );
}

#[test]
fn element_segment_with_expression_items_is_rejected() {
    // The expression form `(elem ... funcref (ref.func $f))` (as opposed to the
    // function-index form `(elem ... $f)`) is not supported.
    assert_unsupported(
        r#"(module (table 1 funcref) (func $f) (elem (i32.const 0) funcref (ref.func $f)))"#,
        "element segment with expression items",
    );
}

#[test]
fn imported_tag_is_rejected() {
    // Tags (exception handling) are not supported as imports.
    assert_unsupported(r#"(module (import "e" "t" (tag)))"#, "imported tag");
}

#[test]
fn multi_value_import_result_is_rejected() {
    // Defined functions may return multiple values, but an imported function
    // that returns more than one value is not supported yet.
    assert_unsupported(
        r#"(module (import "e" "f" (func (result i32 i32))))"#,
        "multi-value import result",
    );
}

#[test]
fn unsupported_operator_is_rejected_with_the_operator_named() {
    // A SIMD instruction is not implemented; the error names the operator so the
    // gap is diagnosable rather than a generic failure.
    assert_unsupported(
        r#"(module (func (result v128) (v128.const i32x4 0 0 0 0)))"#,
        "V128Const",
    );
}

#[test]
fn negative_element_offset_is_rejected() {
    // An active element segment whose `i32.const` offset is negative cannot be a
    // `u32` table index.
    assert_unsupported(
        r#"(module (table 1 funcref) (func $f) (elem (i32.const -1) $f))"#,
        "negative table offset",
    );
}

#[test]
fn non_constant_element_offset_is_rejected() {
    // The element offset must be a plain `i32.const`; a `global.get` offset
    // (permitted by wasm) is not supported.
    assert_unsupported(
        r#"(module
             (import "e" "g" (global i32))
             (table 1 funcref) (func $f)
             (elem (global.get 0) $f))"#,
        "element offset",
    );
}

#[test]
fn non_constant_global_initializer_is_rejected() {
    // A global initialised from another (imported) global via `global.get` is a
    // valid constant expression in wasm, but wasm2rs only supports literal
    // initializers.
    assert_unsupported(
        r#"(module
             (import "e" "g" (global i32))
             (global i32 (global.get 0)))"#,
        "global initializer",
    );
}
