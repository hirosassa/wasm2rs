//! End-to-end tests for the tail-call proposal (`return_call`,
//! `return_call_indirect`).
//!
//! These lower to an ordinary Rust call in tail position (`return <call>;`).
//! Semantics are exact; the one thing not provided is a constant-stack
//! guarantee (Rust has no guaranteed TCO), matching wasm2c's default. The
//! tests therefore pin *correctness* — results, multi-value, zero-value, and
//! indirect dispatch — at moderate recursion depths, not unbounded iteration.

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

mod common;

use common::compile_run;

#[test]
fn return_call_mutual_recursion_is_exact() {
    // Two functions tail-call each other to decide parity. `return_call` must
    // transfer control to the callee and yield its result as this function's
    // result, so `is_even(n)` alternates correctly across many hops.
    compile_run(
        "tail_return_call_parity",
        r#"(module
            (func $is_even (param i32) (result i32)
              local.get 0
              i32.eqz
              if (result i32)
                i32.const 1
              else
                local.get 0
                i32.const 1
                i32.sub
                return_call $is_odd
              end)
            (func $is_odd (param i32) (result i32)
              local.get 0
              i32.eqz
              if (result i32)
                i32.const 0
              else
                local.get 0
                i32.const 1
                i32.sub
                return_call $is_even
              end)
            (export "is_even" (func $is_even)))"#,
        // Stateless module (no memory/globals/table) -> free functions.
        "assert_eq!(func0(0), 1); \
         assert_eq!(func0(1), 0); \
         assert_eq!(func0(1000), 1); \
         assert_eq!(func0(1001), 0);",
    );
}

#[test]
fn return_call_indirect_dispatches_through_table() {
    // `return_call_indirect` pops the table index, resolves the funcref, and
    // tail-calls it. Index 0 -> add10, index 1 -> double; the dispatcher's
    // result is exactly the callee's result.
    compile_run(
        "tail_return_call_indirect",
        r#"(module
            (type $unary (func (param i32) (result i32)))
            (table 2 funcref)
            (elem (i32.const 0) $add10 $double)
            (func $add10 (param i32) (result i32)
              local.get 0 i32.const 10 i32.add)
            (func $double (param i32) (result i32)
              local.get 0 i32.const 2 i32.mul)
            (func $dispatch (param i32 i32) (result i32)
              local.get 0
              local.get 1
              return_call_indirect (type $unary))
            (export "dispatch" (func $dispatch)))"#,
        "let mut inst = Instance::new(); \
         assert_eq!(inst.func2(5, 0), 15); \
         assert_eq!(inst.func2(5, 1), 10);",
    );
}

#[test]
fn return_call_forwards_multiple_results() {
    // A `return_call` to a multi-value callee forwards the whole tuple as this
    // function's result.
    compile_run(
        "tail_return_call_multivalue",
        r#"(module
            (func $pair (result i32 i32)
              i32.const 3 i32.const 4)
            (func $get (result i32 i32)
              return_call $pair)
            (export "get" (func $get)))"#,
        // Stateless module (no memory/globals/table) -> free functions.
        "assert_eq!(func1(), (3, 4));",
    );
}

#[test]
fn return_call_with_no_results_runs_callee_for_effect() {
    // A void `return_call` transfers control to a void callee purely for its
    // side effect; the caller returns whatever the callee returns (nothing).
    compile_run(
        "tail_return_call_void",
        r#"(module
            (global $g (mut i32) (i32.const 0))
            (func $set
              i32.const 42
              global.set $g)
            (func $go
              return_call $set)
            (func $read (result i32)
              global.get $g)
            (export "go" (func $go))
            (export "read" (func $read)))"#,
        "let mut inst = Instance::new(); \
         inst.func1(); \
         assert_eq!(inst.func2(), 42);",
    );
}
