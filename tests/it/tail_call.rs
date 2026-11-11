//! End-to-end tests for the tail-call proposal (`return_call`,
//! `return_call_indirect`).
//!
//! A `return_call*` to a function that itself tail-calls is parked in a per-call
//! trampoline slot and run by the caller's wrapper, and a self `return_call`
//! loops in place, so chains of tail calls run in constant stack (the
//! `*_constant_stack` tests run a million hops on the default stack at
//! opt-level 0). The remaining tests pin correctness: results, multi-value,
//! zero-value and indirect dispatch.

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

const PARITY_WAT: &str = r#"(module
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
    (export "is_even" (func $is_even)))"#;

#[test]
fn mutual_return_call_runs_in_constant_stack() {
    // The tail-call proposal guarantees constant stack. 1,000,000 hops at
    // opt-level=0 on the default 8 MiB main-thread stack overflow unless the
    // generated code trampolines instead of recursing.
    compile_run(
        "tail_mutual_constant_stack",
        PARITY_WAT,
        "assert_eq!(func0(1_000_000), 1); assert_eq!(func0(1_000_001), 0);",
    );
}

#[test]
fn self_return_call_runs_in_constant_stack() {
    // A countdown that tail-calls itself with an accumulator.
    compile_run(
        "tail_self_constant_stack",
        r#"(module
            (func $sum (param i64 i64) (result i64)
              local.get 0
              i64.eqz
              if (result i64)
                local.get 1
              else
                local.get 0
                i64.const 1
                i64.sub
                local.get 1
                local.get 0
                i64.add
                return_call $sum
              end)
            (export "sum" (func $sum)))"#,
        "assert_eq!(func0(2_000_000, 0), 2_000_001_000_000);",
    );
}

#[test]
fn return_call_indirect_runs_in_constant_stack() {
    // Parity through a table: each hop resolves a funcref, so the trampolined
    // dispatch must not nest.
    compile_run(
        "tail_indirect_constant_stack",
        r#"(module
            (type $t (func (param i32) (result i32)))
            (table 2 funcref)
            (elem (i32.const 0) $even $odd)
            (func $even (type $t)
              local.get 0
              i32.eqz
              if (result i32)
                i32.const 1
              else
                local.get 0
                i32.const 1
                i32.sub
                i32.const 1
                return_call_indirect (type $t)
              end)
            (func $odd (type $t)
              local.get 0
              i32.eqz
              if (result i32)
                i32.const 0
              else
                local.get 0
                i32.const 1
                i32.sub
                i32.const 0
                return_call_indirect (type $t)
              end)
            (export "even" (func $even)))"#,
        "let mut inst = Instance::new(); \
         assert_eq!(inst.func0(1_000_000), 1); \
         assert_eq!(inst.func0(1_000_001), 0);",
    );
}

#[test]
fn return_call_ref_runs_in_constant_stack() {
    compile_run(
        "tail_call_ref_constant_stack",
        r#"(module
            (type $t (func (param i32) (result i32)))
            (elem declare func $even $odd)
            (func $even (type $t)
              local.get 0
              i32.eqz
              if (result i32)
                i32.const 1
              else
                local.get 0
                i32.const 1
                i32.sub
                ref.func $odd
                return_call_ref $t
              end)
            (func $odd (type $t)
              local.get 0
              i32.eqz
              if (result i32)
                i32.const 0
              else
                local.get 0
                i32.const 1
                i32.sub
                ref.func $even
                return_call_ref $t
              end)
            (export "even" (func $even)))"#,
        "let mut inst = Instance::new(); \
         assert_eq!(inst.func0(1_000_000), 1); \
         assert_eq!(inst.func0(1_000_001), 0);",
    );
}

#[test]
fn multi_value_return_call_chain_runs_in_constant_stack() {
    // Both results travel through the parked call unchanged.
    compile_run(
        "tail_multivalue_constant_stack",
        r#"(module
            (func $a (param i32 i32) (result i32 i32)
              local.get 0
              i32.eqz
              if (result i32 i32)
                i32.const 7
                local.get 1
              else
                local.get 0
                i32.const 1
                i32.sub
                local.get 1
                i32.const 1
                i32.add
                return_call $b
              end)
            (func $b (param i32 i32) (result i32 i32)
              local.get 0
              local.get 1
              return_call $a)
            (export "a" (func $a)))"#,
        "assert_eq!(func0(500_000, 0), (7, 500_000));",
    );
}

#[test]
fn return_call_inside_try_body_still_returns_the_callee_result() {
    // A `return_call` in a `try_table` body leaves through the return-escape
    // path; the parked callee must still run once the function is left.
    compile_run(
        "tail_in_try_body",
        r#"(module
            (tag $e)
            (func $leaf (param i32) (result i32)
              local.get 0
              i32.eqz
              if (result i32)
                i32.const 42
              else
                local.get 0
                i32.const 1
                i32.sub
                return_call $loop
              end)
            (func $loop (param i32) (result i32)
              block $h
                try_table (catch_all $h)
                  local.get 0
                  return_call $leaf
                end
              end
              i32.const -1)
            (export "loop" (func $loop)))"#,
        "assert_eq!(func1(300_000), 42);",
    );
}

#[test]
fn return_call_in_try_body_propagates_callee_exception_regardless_of_callee_tail_calls() {
    // `return_call` replaces the caller's frame, so an exception thrown by the
    // callee must not be caught by the handler of the `try_table` the call sits
    // in. That has to hold whether or not the callee tail-calls itself: `$plain`
    // has no `_tc` body, `$tailing` has one.
    compile_run(
        "tail_in_try_body_throw",
        r#"(module
            (tag $e)
            (func $plain (param i32) (result i32) (throw $e))
            (func $tailing (param i32) (result i32)
              local.get 0
              i32.eqz
              if (result i32)
                throw $e
              else
                local.get 0
                i32.const 1
                i32.sub
                return_call $tailing
              end)
            (func $via_plain (param i32) (result i32)
              block $h
                try_table (catch_all $h)
                  local.get 0
                  return_call $plain
                end
              end
              i32.const -1)
            (func $via_tailing (param i32) (result i32)
              block $h
                try_table (catch_all $h)
                  local.get 0
                  return_call $tailing
                end
              end
              i32.const -1)
            (export "via_plain" (func $via_plain))
            (export "via_tailing" (func $via_tailing)))"#,
        "std::panic::set_hook(Box::new(|_| {}));\n\
         assert!(std::panic::catch_unwind(|| func2(3)).is_err());\n\
         assert!(std::panic::catch_unwind(|| func3(3)).is_err());",
    );
}

#[test]
fn return_call_indirect_in_try_body_propagates_callee_exception() {
    // The indirect form must not let the caller's handler catch the callee's
    // exception either, even when no table target tail-calls.
    compile_run(
        "tail_indirect_in_try_body_throw",
        r#"(module
            (tag $e)
            (type $t (func (param i32) (result i32)))
            (table 1 funcref)
            (elem (i32.const 0) $thrower)
            (func $thrower (param i32) (result i32) (throw $e))
            (func $via (param i32) (result i32)
              block $h
                try_table (catch_all $h)
                  local.get 0
                  i32.const 0
                  return_call_indirect (type $t)
                end
              end
              i32.const -1)
            (export "via" (func $via)))"#,
        "std::panic::set_hook(Box::new(|_| {}));\n\
         assert!(std::panic::catch_unwind(|| Instance::new().func1(3)).is_err());",
    );
}
