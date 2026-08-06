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
fn memory64_defined_memory_is_rejected() {
    // 64-bit memory (memory64 proposal) parses but is not supported. Shared
    // memory, by contrast, is now accepted (see tests/atomics.rs).
    assert_unsupported(r#"(module (memory i64 1))"#, "64-bit memory");
}

#[test]
fn memory64_imported_memory_is_rejected() {
    // Same rule on the import path (classify_import), a distinct branch.
    assert_unsupported(
        r#"(module (import "e" "m" (memory i64 1)))"#,
        "64-bit memory",
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
fn try_table_is_rejected() {
    // Only the legacy exception-handling proposal is lowered; the newer
    // `try_table` form has no translation yet.
    assert_unsupported(
        r#"(module
            (tag $e)
            (func (result i32)
              (block $b
                try_table (result i32) (catch_all $b)
                  i32.const 1
                end)))"#,
        "operator",
    );
}

#[test]
fn br_table_out_of_try_body_is_rejected() {
    // A `br` and `br_if` escaping a `try` body are lowered via a closure-outcome
    // signal, but a `br_table` whose arms leave the body (mixing escaping and
    // non-escaping targets) is not lowered yet.
    assert_unsupported(
        r#"(module
            (tag $e)
            (func (param i32)
              (block $out
                try
                  local.get 0
                  br_table $out $out
                catch_all
                end)))"#,
        "br_table out of a try region",
    );
}

#[test]
fn branch_to_try_from_handler_is_rejected() {
    // A `br` to the try's own label from a catch handler would break out of the
    // landing-pad `match`, which is not a loop.
    assert_unsupported(
        r#"(module
            (tag $e)
            (func
              try
                throw $e
              catch_all
                br 0
              end))"#,
        "branch out of a try handler",
    );
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
    // A not-yet-implemented operator (here a tail call from the tail-call
    // proposal) is rejected with the operator named, so the gap is diagnosable
    // rather than a generic failure. The whole SIMD/v128 proposal, including
    // relaxed SIMD, is now implemented (see tests/simd.rs), so this points at a
    // non-SIMD feature instead.
    assert_unsupported(
        r#"(module
            (func $f (result i32) (i32.const 1))
            (func (result i32) (return_call $f)))"#,
        "ReturnCall",
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

#[test]
fn memory_instruction_without_a_memory_section_is_rejected() {
    // A body that touches linear memory in a module that declares none must be
    // rejected rather than emitting code that references a non-existent field.
    assert_unsupported(
        r#"(module (func (result i32) i32.const 0 i32.load))"#,
        "memory instruction without a memory section",
    );
    // `memory.size` reaches the same guard through a different opcode.
    assert_unsupported(
        r#"(module (func (result i32) memory.size))"#,
        "memory instruction without a memory section",
    );
}

#[test]
fn table_instruction_without_a_table_section_is_rejected() {
    assert_unsupported(
        r#"(module (func (result funcref) i32.const 0 table.get 0))"#,
        "table instruction without a table section",
    );
}

#[test]
fn set_of_an_immutable_global_is_rejected() {
    // `global.set` on a non-`mut` global is invalid; wasm2rs rejects it with a
    // specific reason rather than generating an assignment to an immutable field.
    assert_unsupported(
        r#"(module (global i32 (i32.const 1)) (func i32.const 2 global.set 0))"#,
        "set of immutable global",
    );
}

#[test]
fn non_function_composite_type_is_rejected() {
    // GC struct/array types parse but wasm2rs only lowers function types.
    assert_unsupported(
        r#"(module (type (struct (field i32))))"#,
        "non-function composite type",
    );
    assert_unsupported(
        r#"(module (type (array i32)))"#,
        "non-function composite type",
    );
}

#[test]
fn ref_null_of_a_non_reference_heaptype_is_rejected() {
    // Only `funcref` and `externref` nulls are lowered (both to `u32::MAX`). A
    // `ref.null` of a GC/exception abstract heaptype parses but has no `u32`
    // representation, so it is rejected by name rather than mistranslated.
    assert_unsupported(
        r#"(module (func (result i32) (ref.is_null (ref.null exn))))"#,
        "ref.null of unsupported type",
    );
    assert_unsupported(
        r#"(module (func (result i32) (ref.is_null (ref.null any))))"#,
        "ref.null of unsupported type",
    );
}

#[test]
fn call_indirect_without_a_table_is_rejected() {
    // `call_indirect` needs a table to resolve the callee. A module that uses it
    // with no table defined still assembles as valid wasm, but wasm2rs has
    // nothing to dispatch through, so it rejects rather than emitting a call into
    // a non-existent table. (The sibling "non-zero table index" guard is instead
    // shadowed by the earlier "multiple tables" rejection, so it is unreachable
    // from valid input and left untested by design.)
    assert_unsupported(
        r#"(module
             (type $t (func))
             (func (call_indirect (type $t) (i32.const 0))))"#,
        "call_indirect without a table",
    );
}
