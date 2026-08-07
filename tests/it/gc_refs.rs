//! End-to-end tests for the heap-free slice of the GC / typed-function-
//! references proposals (phase 4a): `call_ref` / `return_call_ref`, the `i31`
//! operations, and typed function references stored in locals. References stay
//! represented as a `u32` (function index; `u32::MAX` is null), and an `i31ref`
//! is a 31-bit integer held in an `i32`, so no managed heap is involved yet.

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

#[test]
fn call_ref_invokes_the_funcref_on_the_stack() {
    // `call_ref` pops a funcref and calls it, like `call_indirect` but taking the
    // reference directly from the stack instead of a table slot. This module has
    // no memory/table/globals, so `call_ref` alone must force it to become a
    // method-bearing `Instance` (the dispatch lives on `self`).
    compile_run(
        "gc_call_ref",
        r#"(module
            (type $ft (func (param i32) (result i32)))
            (func $add1 (param i32) (result i32)
              local.get 0 i32.const 1 i32.add)
            (elem declare func $add1)
            (func (export "run") (param i32) (result i32)
              local.get 0
              ref.func $add1
              call_ref $ft))"#,
        "let mut inst = Instance::new(); assert_eq!(inst.func1(41), 42);",
    );
}

#[test]
fn return_call_ref_tail_calls_the_funcref() {
    // `return_call_ref` is the tail-call form: it forwards the funcref's result
    // as this function's result.
    compile_run(
        "gc_return_call_ref",
        r#"(module
            (type $ft (func (param i32) (result i32)))
            (func $dbl (param i32) (result i32)
              local.get 0 i32.const 2 i32.mul)
            (elem declare func $dbl)
            (func (export "run") (param i32) (result i32)
              local.get 0
              ref.func $dbl
              return_call_ref $ft))"#,
        "let mut inst = Instance::new(); assert_eq!(inst.func1(21), 42);",
    );
}

#[test]
fn typed_funcref_round_trips_through_a_local() {
    // A `(ref null $ft)` local holds a typed function reference; `call_ref` on it
    // dispatches. This pins the value-type lowering of a concrete typed funcref
    // (it must render as `u32`, like the abstract `funcref`).
    compile_run(
        "gc_typed_funcref_local",
        r#"(module
            (type $ft (func (param i32) (result i32)))
            (func $add1 (param i32) (result i32)
              local.get 0 i32.const 1 i32.add)
            (elem declare func $add1)
            (func (export "run") (param i32) (result i32)
              (local $f (ref null $ft))
              ref.func $add1
              local.set $f
              local.get 0
              local.get $f
              call_ref $ft))"#,
        "let mut inst = Instance::new(); assert_eq!(inst.func1(41), 42);",
    );
}

#[test]
fn i31_get_s_and_get_u_extend_the_31_bit_payload() {
    // `ref.i31` narrows an i32 to 31 bits; `i31.get_s` sign-extends bit 30 while
    // `i31.get_u` zero-extends. So -1 becomes 0x7FFF_FFFF as a payload, read back
    // as -1 (signed) or 0x7FFF_FFFF (unsigned); a small positive value is
    // unchanged either way. This module is stateless -> free functions.
    compile_run(
        "gc_i31",
        r#"(module
            (func (export "get_s") (param i32) (result i32)
              local.get 0 ref.i31 i31.get_s)
            (func (export "get_u") (param i32) (result i32)
              local.get 0 ref.i31 i31.get_u))"#,
        "assert_eq!(func0(-1), -1); \
         assert_eq!(func1(-1), 0x7FFF_FFFF); \
         assert_eq!(func0(5), 5); \
         assert_eq!(func1(5), 5); \
         assert_eq!(func0(0x4000_0000u32 as i32), 0xC000_0000u32 as i32);",
    );
}
