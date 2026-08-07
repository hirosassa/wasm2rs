//! End-to-end tests for GC phase G1: unifying the `i31` value model with the
//! managed `GcRef` heap so an `i31ref` can flow through an `anyref`/`eqref`.
//!
//! Before this phase an `i31ref` rode the operand stack as a plain `i32`, which
//! could not be stored into an `anyref` slot (a `GcRef`) nor distinguished from a
//! struct by `ref.test`. Now `ref.i31` produces a `GcRef::I31` handle, so an i31
//! and a struct coexist in the `any` hierarchy: `ref.test`/`ref.cast` recognise
//! `i31`/`eq`/`any` targets, and `ref.eq` compares i31 payloads.

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

use common::{compile_run, expect_trap};

#[test]
fn i31_round_trips_through_an_anyref_local() {
    // An `i31ref` stored into a `(ref null any)` local and read back via
    // `ref.cast (ref i31)` preserves its payload, so the value model is unified
    // (the local is a `GcRef`, and `ref.i31` must produce one).
    compile_run(
        "gc_i31_anyref_local",
        r#"(module
            (func (export "f") (param i32) (result i32)
              (local $a (ref null any))
              local.get 0 ref.i31 local.set $a
              local.get $a ref.cast (ref i31) i31.get_s))"#,
        "assert_eq!(func0(-1), -1); assert_eq!(func0(5), 5); \
         assert_eq!(func0(0x4000_0000u32 as i32), 0xC000_0000u32 as i32);",
    );
}

#[test]
fn ref_test_i31_distinguishes_i31_from_struct_in_an_anyref() {
    // `ref.test (ref i31)` on an `anyref` is 1 for an i31 payload and 0 for a
    // struct object, so the two kinds are separable at runtime.
    compile_run(
        "gc_i31_ref_test",
        r#"(module
            (type $s (struct (field i32)))
            (func $classify (param (ref null any)) (result i32)
              local.get 0 ref.test (ref i31))
            (func (export "onI31") (result i32)
              i32.const 5 ref.i31 call $classify)
            (func (export "onStruct") (result i32)
              i32.const 5 struct.new $s call $classify))"#,
        "assert_eq!(func1(), 1); assert_eq!(func2(), 0);",
    );
}

#[test]
fn i31_is_a_subtype_of_eq_and_any() {
    // An i31 answers `ref.test (ref eq)` and `ref.test (ref any)` with 1: it sits
    // under both abstract heap types.
    compile_run(
        "gc_i31_subtyping",
        r#"(module
            (func (export "iseq") (result i32)
              i32.const 3 ref.i31 ref.test (ref eq))
            (func (export "isany") (result i32)
              i32.const 3 ref.i31 ref.test (ref any)))"#,
        "assert_eq!(func0(), 1); assert_eq!(func1(), 1);",
    );
}

#[test]
fn ref_eq_compares_i31_payloads() {
    // `ref.eq` on two i31 handles compares their payloads: equal payloads are 1,
    // distinct payloads 0.
    compile_run(
        "gc_i31_ref_eq",
        r#"(module
            (func (export "eq") (result i32)
              i32.const 7 ref.i31 i32.const 7 ref.i31 ref.eq)
            (func (export "ne") (result i32)
              i32.const 7 ref.i31 i32.const 8 ref.i31 ref.eq))"#,
        "assert_eq!(func0(), 1); assert_eq!(func1(), 0);",
    );
}

#[test]
fn ref_cast_i31_traps_on_a_struct() {
    // Casting a struct object to `(ref i31)` traps: it is not an i31.
    expect_trap(
        "gc_i31_cast_trap",
        r#"(module
            (type $s (struct (field i32)))
            (func (export "cf")
              i32.const 5 struct.new $s ref.cast (ref i31) drop))"#,
        "func0();",
    );
}
