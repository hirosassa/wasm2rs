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
fn checkpoint_combined_with_a_region_suspend() {
    // A cross-call checkpoint AND a region-crossing suspend in the same body
    // (P5b-2b, the last remaining gap). `$outer` suspends inside a `block` —
    // yielding 10 — which forces the flat `pc`-machine lowering; then, after the
    // resume, it `call`s `$inner` as a tail checkpoint. `$inner` itself suspends
    // (yielding 7) before returning 3, so its suspend must unwind through
    // `$outer`'s checkpoint state up to the top-level resumer, and resuming must
    // re-enter that state and drive `$inner` to completion. `$inner`'s return (3)
    // becomes `$outer`'s result. The driver accumulates 10 + 7 + 3 = 20.
    compile_run(
        "cont_checkpoint_in_region_body",
        r#"(module
            (type $ft (func (result i32)))
            (type $ct (cont $ft))
            (tag $yield (param i32))
            (func $outer (result i32)
              (block $b
                i32.const 10 suspend $yield)
              call $inner)
            (func $inner (result i32)
              i32.const 7 suspend $yield
              i32.const 3)
            (func (export "run") (result i32)
              (local $acc i32) (local $k (ref null $ct))
              ref.func $outer cont.new $ct local.set $k
              (loop $again
                (block $on_yield (result i32 (ref $ct))
                  local.get $k resume $ct (on $yield $on_yield)
                  local.get $acc i32.add return)
                local.set $k
                local.get $acc i32.add local.set $acc
                br $again)
              unreachable))"#,
        // `$outer` is func0 (a step function), `$inner` func1, `run` is func2.
        "let mut inst = Instance::new(); assert_eq!(inst.func2(), 20);",
    );
}

#[test]
fn statement_before_a_flat_checkpoint_runs_once() {
    // A side-effecting statement immediately before a flat checkpoint must run
    // exactly once, even though the callee suspends and the checkpoint state is
    // re-entered on resume (P5b-2b). `$outer` suspends inside a `block` (yielding
    // 5, forcing the flat path), then bumps a local `$c` by 1 and `call`s `$inner`
    // (which yields 7 before returning 3). The `$c += 1` sits in the same pc-state
    // as the checkpoint; if the state re-ran it on the callee-suspend re-entry,
    // `$c` would reach 2. `$outer` returns `$inner`'s result + `$c` = 3 + 1 = 4, so
    // the driver accumulates 5 + 7 + 4 = 16 (a re-run would give 17).
    compile_run(
        "cont_stmt_before_checkpoint",
        r#"(module
            (type $ft (func (result i32)))
            (type $ct (cont $ft))
            (tag $yield (param i32))
            (func $outer (result i32)
              (local $c i32)
              (block $b
                i32.const 5 suspend $yield)
              local.get $c i32.const 1 i32.add local.set $c
              call $inner
              local.get $c i32.add)
            (func $inner (result i32)
              i32.const 7 suspend $yield
              i32.const 3)
            (func (export "run") (result i32)
              (local $acc i32) (local $k (ref null $ct))
              ref.func $outer cont.new $ct local.set $k
              (loop $again
                (block $on_yield (result i32 (ref $ct))
                  local.get $k resume $ct (on $yield $on_yield)
                  local.get $acc i32.add return)
                local.set $k
                local.get $acc i32.add local.set $acc
                br $again)
              unreachable))"#,
        "let mut inst = Instance::new(); assert_eq!(inst.func2(), 16);",
    );
}

#[test]
fn checkpoint_result_survives_a_later_region_suspend() {
    // A checkpoint whose result then flows through a *later* region suspend
    // (P5b-2b). `$outer` first `call`s `$inner` as a tail-shaped checkpoint —
    // `$inner` yields 7, then returns 3 — and feeds that 3 as the parameter of a
    // `block` that suspends (yielding 100) before adding 5. So `$inner`'s result
    // must first land in `__frame.ostack` (checkpoint return), then survive the
    // region suspend as an operand re-saved into `__frame.ostack`, and finally be
    // read back to compute 3 + 5 = 8. The driver accumulates 7 + 100 + 8 = 115.
    compile_run(
        "cont_checkpoint_then_region_suspend",
        r#"(module
            (type $ft (func (result i32)))
            (type $ct (cont $ft))
            (tag $yield (param i32))
            (func $outer (result i32)
              call $inner
              (block $b (param i32) (result i32)
                i32.const 100 suspend $yield
                i32.const 5 i32.add))
            (func $inner (result i32)
              i32.const 7 suspend $yield
              i32.const 3)
            (func (export "run") (result i32)
              (local $acc i32) (local $k (ref null $ct))
              ref.func $outer cont.new $ct local.set $k
              (loop $again
                (block $on_yield (result i32 (ref $ct))
                  local.get $k resume $ct (on $yield $on_yield)
                  local.get $acc i32.add return)
                local.set $k
                local.get $acc i32.add local.set $acc
                br $again)
              unreachable))"#,
        "let mut inst = Instance::new(); assert_eq!(inst.func2(), 115);",
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
fn continuation_body_parameter_is_delivered_at_first_resume() {
    // A continuation body with a parameter (P5b). `$gen` takes `$n` and returns
    // `$n + 100`. `cont.new` builds the continuation and the *first* `resume`
    // supplies the argument (5), which the body must load into its parameter
    // local. The body never suspends, so `resume` (no handlers) returns
    // immediately with 5 + 100 = 105. If the argument were dropped, the
    // parameter would read as its zero default and the result would be 100.
    compile_run(
        "cont_param_first_resume",
        r#"(module
            (type $ft (func (param i32) (result i32)))
            (type $ct (cont $ft))
            (func $gen (param $n i32) (result i32)
              local.get $n i32.const 100 i32.add)
            (func (export "run") (result i32)
              i32.const 5
              ref.func $gen cont.new $ct
              resume $ct))"#,
        // `$gen` is func0 (a step function); the exported `run` is func1.
        "let mut inst = Instance::new(); assert_eq!(inst.func1(), 105);",
    );
}

#[test]
fn continuation_parameter_survives_a_suspend() {
    // The parameter must persist across a suspend like any other local (P5b).
    // `$gen($n)` yields `$n` (5), then — after the resume — returns `$n + 100`
    // (105). The driver resumes with the argument 5, accumulates the yielded 5,
    // then resumes the (now parameter-less) reused continuation to its return
    // (105), summing 5 + 105 = 110.
    compile_run(
        "cont_param_suspend",
        r#"(module
            (type $ft (func (param i32) (result i32)))
            (type $ct (cont $ft))
            (type $ft0 (func (result i32)))
            (type $ct0 (cont $ft0))
            (tag $yield (param i32))
            (func $gen (param $n i32) (result i32)
              local.get $n suspend $yield
              local.get $n i32.const 100 i32.add)
            (func (export "run") (result i32)
              (local $acc i32) (local $k (ref null $ct0))
              (block $on_yield (result i32 (ref $ct0))
                i32.const 5 ref.func $gen cont.new $ct
                resume $ct (on $yield $on_yield)
                return)
              local.set $k
              local.get $acc i32.add local.set $acc
              (block $on_yield2 (result i32 (ref $ct0))
                local.get $k
                resume $ct0 (on $yield $on_yield2)
                local.get $acc i32.add return)
              unreachable))"#,
        // `$gen` is func0 (a step function); the exported `run` is func1.
        "let mut inst = Instance::new(); assert_eq!(inst.func1(), 110);",
    );
}

#[test]
fn resume_sends_a_value_back_into_a_suspend() {
    // A bidirectional generator (P5b): the tag `$ch` carries a parameter (the
    // value yielded up) *and* a result (the value sent back down on resume).
    // `$gen` yields 10 via `suspend $ch`, and that suspend then evaluates to the
    // value the next `resume` injects; it adds 100 and returns. The driver's
    // first `resume` (no args) drives to the suspend; the second `resume` sends
    // 5 back, so `$gen` returns 5 + 100 = 105. If the sent value were not
    // injected, the suspend would leave nothing on the stack and the add would
    // underflow — so this exercises the result-injection path end to end.
    //
    // The reused continuation has a *different* type ($ct2, whose underlying
    // function takes the injected i32) than the fresh one ($ct), mirroring how
    // a suspend re-types the continuation to expect the tag's results.
    compile_run(
        "cont_resume_send",
        r#"(module
            (type $ft (func (result i32)))
            (type $ct (cont $ft))
            (type $ft2 (func (param i32) (result i32)))
            (type $ct2 (cont $ft2))
            (tag $ch (param i32) (result i32))
            (func $gen (result i32)
              i32.const 10 suspend $ch
              i32.const 100 i32.add)
            (func (export "run") (result i32)
              (local $k (ref null $ct2))
              (block $on_ch (result i32 (ref $ct2))
                ref.func $gen cont.new $ct
                resume $ct (on $ch $on_ch)
                return)
              local.set $k
              drop
              i32.const 5
              local.get $k
              resume $ct2))"#,
        // `$gen` is func0 (a step function); the exported `run` is func1.
        "let mut inst = Instance::new(); assert_eq!(inst.func1(), 105);",
    );
}

#[test]
fn nested_if_in_a_continuation_body_without_suspend() {
    // A continuation body may contain nested structured control flow as long as
    // no `suspend` crosses it (P5b-2a). Here `$gen` selects 7 via an `if`
    // (no suspend anywhere), then adds 100 and returns 107. The `if` region is a
    // single straight-line chunk inside the (only) pc state; a single `resume`
    // (no handlers) drives it straight to its return.
    compile_run(
        "cont_nested_if_no_suspend",
        r#"(module
            (type $ft (func (result i32)))
            (type $ct (cont $ft))
            (func $gen (result i32)
              i32.const 1
              (if (result i32)
                (then i32.const 7)
                (else i32.const 0))
              i32.const 100
              i32.add)
            (func (export "run") (result i32)
              ref.func $gen cont.new $ct
              resume $ct))"#,
        // `$gen` is func0 (a step function); the exported `run` is func1.
        "let mut inst = Instance::new(); assert_eq!(inst.func1(), 107);",
    );
}

#[test]
fn nested_block_after_a_suspend_in_a_continuation_body() {
    // Nested control flow in a *non-initial* pc state (P5b-2a). `$gen` yields 10,
    // then — after the resume — computes 20 inside a `block` (no suspend inside)
    // and returns 20 + 5 = 25. The driver resumes until it returns, accumulating
    // 10 + 25 = 35. This exercises a region rendered into an arm that runs after
    // a suspend boundary (pc > 0).
    compile_run(
        "cont_block_after_suspend",
        r#"(module
            (type $ft (func (result i32)))
            (type $ct (cont $ft))
            (tag $yield (param i32))
            (func $gen (result i32)
              i32.const 10 suspend $yield
              (block $b (result i32) i32.const 20)
              i32.const 5 i32.add)
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
        "let mut inst = Instance::new(); assert_eq!(inst.func1(), 35);",
    );
}

#[test]
fn diverging_tail_after_a_suspend_compiles() {
    // A continuation body may diverge before its `end` (here an infinite `loop`
    // in a post-suspend state). The step function must still *compile* under
    // `-D warnings`: the diverging arm emits no trailing `StepResult::Return`
    // (which would be unreachable) and never reads the operand stack (which
    // would underflow). `$gen` yields 42, then — if resumed again — loops
    // forever; the driver resumes it exactly once and keeps the yielded 42, so
    // the diverging arm is compiled but never executed.
    compile_run(
        "cont_diverging_tail",
        r#"(module
            (type $ft (func (result i32)))
            (type $ct (cont $ft))
            (tag $yield (param i32))
            (func $gen (result i32)
              i32.const 42 suspend $yield
              (loop $l br $l))
            (func (export "run") (result i32)
              (local $k (ref null $ct))
              ref.func $gen cont.new $ct local.set $k
              (block $on_yield (result i32 (ref $ct))
                local.get $k resume $ct (on $yield $on_yield)
                return)
              local.set $k))"#,
        // `$gen` is func0 (a step function); the exported `run` is func1.
        "let mut inst = Instance::new(); assert_eq!(inst.func1(), 42);",
    );
}

#[test]
fn suspend_inside_a_block() {
    // A `suspend` *inside* a nested region (P5b-2b). `$gen` yields 10 from inside
    // an (untargeted) `block`, then — after the resume — falls out of the block
    // and returns 30. Because the suspend crosses the region boundary, the region
    // can no longer be rendered as a single straight-line arm: the `pc` state
    // machine must be woven through the block. The driver resumes until the
    // continuation returns, accumulating 10 + 30 = 40.
    compile_run(
        "cont_suspend_in_block",
        r#"(module
            (type $ft (func (result i32)))
            (type $ct (cont $ft))
            (tag $yield (param i32))
            (func $gen (result i32)
              (block $b
                i32.const 10 suspend $yield)
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
fn suspend_inside_a_loop() {
    // A `suspend` inside a back-edge `loop` (P5b-2b). `$gen` counts a local `$i`
    // up, yielding it each iteration, and loops back while `$i < 3`; once `$i`
    // reaches 3 it falls out of the loop and returns 99. This exercises the `pc`
    // machine threaded through a loop: the local `$i` survives each suspend (saved
    // to and reloaded from the frame), and the `br_if` back-edge is a `pc` jump.
    // The driver resumes until it returns, accumulating 1 + 2 + 3 + 99 = 105.
    compile_run(
        "cont_suspend_in_loop",
        r#"(module
            (type $ft (func (result i32)))
            (type $ct (cont $ft))
            (tag $yield (param i32))
            (func $gen (result i32)
              (local $i i32)
              (loop $l
                local.get $i i32.const 1 i32.add local.set $i
                local.get $i suspend $yield
                local.get $i i32.const 3 i32.lt_s br_if $l)
              i32.const 99)
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
        "let mut inst = Instance::new(); assert_eq!(inst.func1(), 105);",
    );
}

#[test]
fn suspend_inside_an_if() {
    // A `suspend` inside an `if` arm that also yields the region's result
    // (P5b-2b). The condition is 1 (true), so the `then` arm runs: it yields 10,
    // then — after the resume — produces 20 as the `if`'s result. Adding 5 gives
    // 25. Because the suspend sits inside the `then` region, the `pc` machine
    // threads through the `if`, and the region result temp (assigned after the
    // resume) is read once control rejoins. The driver resumes until it returns,
    // accumulating 10 + 25 = 35.
    compile_run(
        "cont_suspend_in_if",
        r#"(module
            (type $ft (func (result i32)))
            (type $ct (cont $ft))
            (tag $yield (param i32))
            (func $gen (result i32)
              i32.const 1
              (if (result i32)
                (then i32.const 10 suspend $yield i32.const 20)
                (else i32.const 30))
              i32.const 5 i32.add)
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
        "let mut inst = Instance::new(); assert_eq!(inst.func1(), 35);",
    );
}

#[test]
fn block_param_survives_suspend() {
    // A block *parameter* (P5b-2b remainder). The operand `100` is consumed as the
    // block's entry parameter, so it sits on the operand stack *below* the suspend
    // payload and must survive the suspend inside the region: the `pc` machine
    // saves it into the frame's `ostack` and the resumed state reads it back to
    // compute `100 + 5 = 105`. `$gen` yields 10 then returns 105; the driver
    // accumulates 10 + 105 = 115.
    compile_run(
        "cont_block_param",
        r#"(module
            (type $ft (func (result i32)))
            (type $ct (cont $ft))
            (tag $yield (param i32))
            (func $gen (result i32)
              i32.const 100
              (block $b (param i32) (result i32)
                i32.const 10 suspend $yield
                i32.const 5 i32.add))
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
        "let mut inst = Instance::new(); assert_eq!(inst.func1(), 115);",
    );
}

#[test]
fn two_block_params_survive_suspend_in_order() {
    // Two block parameters survive one suspend, pinning the deepest-first `ostack`
    // ordering (P5b-2b remainder). The block takes `(param i32 i32)` = [100, 7], so
    // `100` is the deeper survivor (`ostack[0]`) and `7` the shallower (`ostack[1]`).
    // After the resume the region computes `i32.sub` = 100 - 7 = 93 — an
    // order-sensitive op, so a swapped save/restore would yield -93 instead. `$gen`
    // yields 10 then returns 93; the driver accumulates 10 + 93 = 103.
    compile_run(
        "cont_two_block_params",
        r#"(module
            (type $ft (func (result i32)))
            (type $ct (cont $ft))
            (tag $yield (param i32))
            (func $gen (result i32)
              i32.const 100
              i32.const 7
              (block $b (param i32 i32) (result i32)
                i32.const 10 suspend $yield
                i32.sub))
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
        "let mut inst = Instance::new(); assert_eq!(inst.func1(), 103);",
    );
}

#[test]
fn loop_param_survives_suspend() {
    // A loop-carried *parameter* that survives a suspend each iteration (P5b-2b
    // remainder). The `loop (param i32) (result i32)` carries an accumulator on the
    // operand stack: each turn it bumps a local `$i`, folds `$i` into the
    // accumulator, then yields `$i` — leaving the accumulator on the stack *below*
    // the yield payload, so it must survive the suspend. While `$i < 3` a `br_if`
    // back-edge carries the updated accumulator as the loop parameter; otherwise it
    // falls through as the loop (and function) result. The accumulator survivor is
    // a non-stable expression, so it exercises the freeze-to-hoisted-temp path.
    // acc: 0->1 (yield 1), ->3 (yield 2), ->6 (yield 3), returns 6. The driver
    // accumulates the three yields plus the return: 1 + 2 + 3 + 6 = 12.
    compile_run(
        "cont_loop_param",
        r#"(module
            (type $ft (func (result i32)))
            (type $ct (cont $ft))
            (tag $yield (param i32))
            (func $gen (result i32)
              (local $i i32)
              i32.const 0
              (loop $l (param i32) (result i32)
                local.get $i i32.const 1 i32.add local.set $i
                local.get $i i32.add
                local.get $i suspend $yield
                local.get $i i32.const 3 i32.lt_s br_if $l))
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
        "let mut inst = Instance::new(); assert_eq!(inst.func1(), 12);",
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
