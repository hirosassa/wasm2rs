//! End-to-end tests for the typed-continuations (stack-switching) proposal.
//!
//! Phase 1 covers the type section and the null continuation reference: a
//! `(cont $ft)` type is accepted (it names an underlying function type), a
//! continuation reference lowers to a `u32` handle (`u32::MAX` is null, like a
//! `funcref`), and `ref.null`/`ref.is_null` work on it. This phase also adds
//! `cont.new`, which turns a `funcref` into a live (non-null) continuation
//! handle. Resuming and suspending continuations arrive in later phases.
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

use common::{compile_run, compile_run_split};

#[test]
fn cont_type_is_accepted_and_null_ref_is_null() {
    // Defining a `(cont $ft)` type must no longer be rejected, and `ref.null`
    // of that continuation type produces a null handle that `ref.is_null`
    // reports as null (1).
    compile_run(
        "cont_null_ref",
        r#"(module
            (type $ft (func (param i32) (result i32)))
            (type $ct (cont $ft))
            (func (export "f") (result i32)
              ref.null $ct ref.is_null))"#,
        "assert_eq!(func0(), 1);",
    );
}

#[test]
fn cont_new_produces_non_null_handle() {
    // `cont.new $ct` consumes the `funcref` from `ref.func $gen` and produces a
    // live continuation handle. `ref.is_null` reports it as non-null (0), in
    // contrast to a `ref.null $ct` handle (which is null, 1).
    compile_run(
        "cont_new_non_null",
        r#"(module
            (type $ft (func (result i32)))
            (type $ct (cont $ft))
            (func $gen (result i32) i32.const 42)
            (func (export "f") (result i32)
              ref.func $gen cont.new $ct ref.is_null))"#,
        // `$gen` is func0; the exported `f` is func1. Creating a continuation
        // makes the module stateful, so the functions are `Instance` methods.
        "let mut inst = Instance::new(); assert_eq!(inst.func1(), 0);",
    );
}

#[test]
fn generator_single_suspend() {
    // A one-shot generator that yields once (10) then returns (30). The driver
    // resumes it in a loop: the first resume suspends with 10, the second
    // returns 30. It accumulates 10 + 30 = 40.
    compile_run(
        "cont_gen_single",
        r#"(module
            (type $ft (func (result i32)))
            (type $ct (cont $ft))
            (tag $yield (param i32))
            (func $gen (result i32)
              i32.const 10 suspend $yield
              i32.const 30)
            (func (export "run") (result i32)
              (local $acc i32) (local $k (ref null $ct))
              ref.func $gen cont.new $ct local.set $k
              (loop $again
                (block $on_yield (result i32 (ref $ct))
                  local.get $k resume $ct (on $yield $on_yield)
                  local.get $acc i32.add return)
                local.set $k
                local.get $acc i32.add local.set $acc
                br $again)
              unreachable))"#,
        // `$gen` is func0 (a step function); the exported `run` is func1.
        "let mut inst = Instance::new(); assert_eq!(inst.func1(), 40);",
    );
}

#[test]
fn generator_two_suspends() {
    // The canonical generator: yields 10, then 20, then returns 30. The driver
    // resumes until the continuation returns, accumulating 10 + 20 + 30 = 60.
    compile_run(
        "cont_gen_two",
        r#"(module
            (type $ft (func (result i32)))
            (type $ct (cont $ft))
            (tag $yield (param i32))
            (func $gen (result i32)
              i32.const 10 suspend $yield
              i32.const 20 suspend $yield
              i32.const 30)
            (func (export "run") (result i32)
              (local $acc i32) (local $k (ref null $ct))
              ref.func $gen cont.new $ct local.set $k
              (loop $again
                (block $on_yield (result i32 (ref $ct))
                  local.get $k resume $ct (on $yield $on_yield)
                  local.get $acc i32.add return)
                local.set $k
                local.get $acc i32.add local.set $acc
                br $again)
              unreachable))"#,
        "let mut inst = Instance::new(); assert_eq!(inst.func1(), 60);",
    );
}

#[test]
fn generator_split_across_files() {
    // The same two-suspend generator, but split one function per file, so the
    // continuation step function lands in a different chunk from its resumer.
    // Both chunks share the same `impl Instance`, so `cont_step` still reaches
    // `cont_step_func0`. Verifies the multi-file path emits the continuation
    // runtime (via the reused module header) correctly.
    compile_run_split(
        "cont_gen_split",
        r#"(module
            (type $ft (func (result i32)))
            (type $ct (cont $ft))
            (tag $yield (param i32))
            (func $gen (result i32)
              i32.const 10 suspend $yield
              i32.const 20 suspend $yield
              i32.const 30)
            (func (export "run") (result i32)
              (local $acc i32) (local $k (ref null $ct))
              ref.func $gen cont.new $ct local.set $k
              (loop $again
                (block $on_yield (result i32 (ref $ct))
                  local.get $k resume $ct (on $yield $on_yield)
                  local.get $acc i32.add return)
                local.set $k
                local.get $acc i32.add local.set $acc
                br $again)
              unreachable))"#,
        1,
        "let mut inst = Instance::new(); assert_eq!(inst.func1(), 60);",
    );
}

#[test]
fn null_cont_local_defaults_to_null() {
    // A local typed `(ref null $ct)` defaults to the null handle, so reading it
    // back and testing `ref.is_null` reports null (1) without any assignment.
    compile_run(
        "cont_null_local",
        r#"(module
            (type $ft (func (result i32)))
            (type $ct (cont $ft))
            (func (export "f") (result i32)
              (local $k (ref null $ct))
              local.get $k ref.is_null))"#,
        "assert_eq!(func0(), 1);",
    );
}
