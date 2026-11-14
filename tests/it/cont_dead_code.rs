//! Dead `suspend`/`switch`/cross-call checkpoint inside a continuation body (#385):
//! after a region nothing leaves, the cont step lowering must skip them like the
//! ordinary operator path, or rustc rejects the output (`unreachable statement`,
//! then a mistyped payload).

use crate::common;

use common::compile_run;

/// A module whose generator `$gen` has `body` and whose export just creates (but
/// never drives) a continuation over it, so the generator is a step function that
/// must compile while the diverging body is never run.
fn module(body: &str) -> String {
    format!(
        r#"(module
            (elem declare func $gen)
            (type $ft (func (result i32)))
            (type $ct (cont $ft))
            (tag $yield (param i32))
            (tag $boom)
            (func $gen (result i32) {body})
            (func (export "run") (result i32)
              ref.func $gen cont.new $ct drop
              i32.const 7))"#
    )
}

fn check(name: &str, body: &str) {
    compile_run(
        name,
        &module(body),
        // `$gen` is func0; the exported `run` is func1.
        "let mut inst = Instance::new(); assert_eq!(inst.func1(), 7);",
    );
}

#[test]
fn suspend_after_infinite_loop() {
    // The reproduction from the issue.
    check(
        "cont_dead_loop_suspend",
        "(loop (result i32) (br 0)) suspend $yield i32.const 2",
    );
}

#[test]
fn suspend_after_throw() {
    check(
        "cont_dead_throw_suspend",
        "throw $boom suspend $yield i32.const 2",
    );
}

#[test]
fn suspend_after_return() {
    check(
        "cont_dead_return_suspend",
        "i32.const 1 return suspend $yield i32.const 2",
    );
}

#[test]
fn suspend_after_br_to_function_label() {
    check(
        "cont_dead_br_suspend",
        "i32.const 1 br 0 suspend $yield i32.const 2",
    );
}

#[test]
fn live_suspend_before_dead_one() {
    // A reachable suspend still works when a dead one follows.
    check(
        "cont_dead_after_live",
        "i32.const 10 suspend $yield (loop (result i32) (br 0)) suspend $yield i32.const 2",
    );
}

#[test]
fn dead_nested_region_with_suspend() {
    // A dead block must not end the dead-skipping early: its `end` closes the
    // dead region, not the function.
    check(
        "cont_dead_nested_region",
        "(loop (result i32) (br 0)) drop (block (result i32) i32.const 1 suspend $yield i32.const 2) suspend $yield i32.const 2",
    );
}

#[test]
fn dead_switch_after_infinite_loop() {
    compile_run(
        "cont_dead_switch",
        r#"(module
            (elem declare func $b $a)
            (type $ht (func (result i32)))
            (tag $t (type $ht))
            (type $fa (func (result i32)))
            (type $ca (cont $fa))
            (type $fa_s (func (result i32)))
            (type $ca_s (cont $fa_s))
            (type $fb (func (param i32) (param (ref $ca_s)) (result i32)))
            (type $cb (cont $fb))
            (func $a (type $fa)
              (loop (br 0))
              i32.const 11
              ref.func $b cont.new $cb
              switch $cb $t
              i32.const 999)
            (func $b (type $fb)
              local.get 0 i32.const 2 i32.add)
            (func (export "run") (type $fa)
              ref.func $a cont.new $ca drop
              i32.const 7))"#,
        // `$a` is func0 and `$b` is func1 (step functions); `run` is func2.
        "let mut inst = Instance::new(); assert_eq!(inst.func2(), 7);",
    );
}

#[test]
fn dead_cross_call_checkpoint_after_infinite_loop() {
    // A call to another step function is a checkpoint when live; dead, it must
    // be skipped like any other call.
    compile_run(
        "cont_dead_checkpoint",
        r#"(module
            (elem declare func $inner $outer)
            (type $ft (func (result i32)))
            (type $ct (cont $ft))
            (tag $yield (param i32))
            (func $inner (result i32) i32.const 1 suspend $yield i32.const 2)
            (func $outer (result i32)
              (loop (br 0))
              call $inner)
            (func (export "run") (result i32)
              ref.func $outer cont.new $ct drop
              ref.func $inner cont.new $ct drop
              i32.const 7))"#,
        // `$inner` is func0, `$outer` func1, and the exported `run` func2.
        "let mut inst = Instance::new(); assert_eq!(inst.func2(), 7);",
    );
}

#[test]
fn dead_empty_block_before_top_level_suspend() {
    // The dead block's `end` must not be taken for the function's: the suspend
    // stays at depth 0 (arm-splitting path), so only the `end` bookkeeping is
    // exercised.
    check(
        "cont_dead_empty_block",
        "(loop (result i32) (br 0)) (block (nop)) suspend $yield i32.const 2",
    );
}
