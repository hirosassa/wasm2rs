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
fn suspend_propagates_across_a_call() {
    // Cross-call suspend propagation (P5). The continuation body `$f` suspends
    // once itself (yielding 1), then `call`s `$g`, which suspends once (yielding
    // 7) before returning 100. `$g`'s suspend must unwind through `$f` up to the
    // top-level resumer, and resuming must re-enter `$f` at the call checkpoint
    // and drive `$g` to completion. The driver accumulates 1 + 7 + 100 = 108.
    compile_run(
        "cont_call_propagate",
        r#"(module
            (type $ft (func (result i32)))
            (type $ct (cont $ft))
            (tag $yield (param i32))
            (func $f (result i32)
              i32.const 1 suspend $yield
              call $g)
            (func $g (result i32)
              i32.const 7 suspend $yield
              i32.const 100)
            (func (export "run") (result i32)
              (local $acc i32) (local $k (ref null $ct))
              ref.func $f cont.new $ct local.set $k
              (loop $again
                (block $on_yield (result i32 (ref $ct))
                  local.get $k resume $ct (on $yield $on_yield)
                  local.get $acc i32.add return)
                local.set $k
                local.get $acc i32.add local.set $acc
                br $again)
              unreachable))"#,
        // `$f` is func0 (a step function), `$g` func1 (also a step function),
        // the exported `run` is func2.
        "let mut inst = Instance::new(); assert_eq!(inst.func2(), 108);",
    );
}

#[test]
fn checkpoint_result_may_be_discarded() {
    // The continuation body `$f` `call`s `$g` (which suspends once, yielding 7,
    // then returns 100) but `drop`s `$g`'s result and returns its own constant
    // (7) instead. This exercises the checkpoint arm's `_`-bound return path (the
    // callee's `StepResult::Return` payload is not read), while still driving
    // `$g` to completion across its suspend. The driver accumulates 7 + 7 = 14.
    compile_run(
        "cont_call_discard",
        r#"(module
            (type $ft (func (result i32)))
            (type $ct (cont $ft))
            (tag $yield (param i32))
            (func $f (result i32)
              call $g
              drop
              i32.const 7)
            (func $g (result i32)
              i32.const 7 suspend $yield
              i32.const 100)
            (func (export "run") (result i32)
              (local $acc i32) (local $k (ref null $ct))
              ref.func $f cont.new $ct local.set $k
              (loop $again
                (block $on_yield (result i32 (ref $ct))
                  local.get $k resume $ct (on $yield $on_yield)
                  local.get $acc i32.add return)
                local.set $k
                local.get $acc i32.add local.set $acc
                br $again)
              unreachable))"#,
        "let mut inst = Instance::new(); assert_eq!(inst.func2(), 14);",
    );
}

#[test]
fn local_survives_a_suspend() {
    // A generator that keeps state in a local across a suspend (P5b). `$gen`
    // stores 10 in `$c`, yields `$c` (10), then — after the resume — reads `$c`
    // back (still 10), adds 5, and returns it (15). For the local to read back
    // as 10 after the suspend, it must be saved into the frame and reloaded on
    // resume rather than living only in a stack variable. The driver accumulates
    // 10 + 15 = 25.
    compile_run(
        "cont_local_state",
        r#"(module
            (type $ft (func (result i32)))
            (type $ct (cont $ft))
            (tag $yield (param i32))
            (func $gen (result i32) (local $c i32)
              i32.const 10 local.set $c
              local.get $c suspend $yield
              local.get $c i32.const 5 i32.add local.set $c
              local.get $c)
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
        "let mut inst = Instance::new(); assert_eq!(inst.func1(), 25);",
    );
}

#[test]
fn local_survives_a_cross_call_suspend() {
    // A local held across both the body's own suspend and a cross-call
    // checkpoint (P5b + P5a together). `$f` stores 3 in `$c`, yields it (3),
    // then `call`s `$g` (which yields 7 and returns 100) and finally returns
    // `$g`'s result plus `$c` (100 + 3 = 103). For that to hold, `$c` must be
    // saved into the frame not only at `$f`'s own suspend but also each time
    // `$g` suspends up through the checkpoint. The driver sums 3 + 7 + 103 = 113.
    compile_run(
        "cont_local_cross_call",
        r#"(module
            (type $ft (func (result i32)))
            (type $ct (cont $ft))
            (tag $yield (param i32))
            (func $f (result i32) (local $c i32)
              i32.const 3 local.set $c
              local.get $c suspend $yield
              call $g
              local.get $c i32.add)
            (func $g (result i32)
              i32.const 7 suspend $yield
              i32.const 100)
            (func (export "run") (result i32)
              (local $acc i32) (local $k (ref null $ct))
              ref.func $f cont.new $ct local.set $k
              (loop $again
                (block $on_yield (result i32 (ref $ct))
                  local.get $k resume $ct (on $yield $on_yield)
                  local.get $acc i32.add return)
                local.set $k
                local.get $acc i32.add local.set $acc
                br $again)
              unreachable))"#,
        "let mut inst = Instance::new(); assert_eq!(inst.func2(), 113);",
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
