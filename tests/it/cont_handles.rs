//! The continuation handle table must not grow without bound: a handle whose
//! continuation has returned is recycled, while a stale reference to it still
//! traps (it must never alias the continuation that reuses the slot).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]

use crate::common::{compile_run, expect_trap_with};

const PRELUDE: &str = r#"
    (elem declare func $gen)
    (type $ft (func (result i32)))
    (type $ct (cont $ft))
    (tag $yield (param i32))
    (func $gen (result i32)
      i32.const 1 suspend $yield
      i32.const 2)
"#;

#[test]
fn consumed_continuations_do_not_grow_the_handle_table() {
    // Each iteration creates a continuation, suspends it once and runs it to
    // completion. 50_000 of them must reuse the same slot rather than push 50_000
    // table entries (a slot is retired once its generation wraps, so a few more).
    let wat = format!(
        r#"(module {PRELUDE}
          (func (export "run") (param $n i32) (result i32)
            (local $k (ref null $ct)) (local $acc i32)
            (loop $again
              (local.set $k (cont.new $ct (ref.func $gen)))
              (block $on_yield (result i32 (ref $ct))
                (resume $ct (on $yield $on_yield) (local.get $k))
                unreachable)
              local.set $k
              drop
              (local.set $acc (i32.add (local.get $acc) (resume $ct (local.get $k))))
              (local.tee $n (i32.sub (local.get $n) (i32.const 1)))
              br_if $again)
            local.get $acc))"#
    );
    compile_run(
        "cont_handles_no_growth",
        &wat,
        "let mut inst = Instance::new(); \
         assert_eq!(inst.func1(50_000), 100_000); \
         assert!(inst.conts.len() <= 64, \"table grew to {}\", inst.conts.len());",
    );
}

#[test]
fn a_stale_handle_still_traps_after_its_slot_is_reused() {
    // `$k1` runs to completion, then `$k2` takes over its table slot. Resuming the
    // stale `$k1` must trap as a consumed continuation, not drive `$k2`.
    let wat = format!(
        r#"(module {PRELUDE}
          (func (export "run") (result i32)
            (local $k1 (ref null $ct)) (local $k2 (ref null $ct))
            (local.set $k1 (cont.new $ct (ref.func $gen)))
            (block $y (result i32 (ref $ct))
              (resume $ct (on $yield $y) (local.get $k1))
              unreachable)
            local.set $k1
            drop
            (drop (resume $ct (local.get $k1)))
            (local.set $k2 (cont.new $ct (ref.func $gen)))
            (resume $ct (local.get $k1))))"#
    );
    expect_trap_with(
        "cont_handles_stale_trap",
        &wat,
        "let mut inst = Instance::new(); inst.func1();",
        "consumed continuation",
    );
}

#[test]
fn a_stale_handle_still_traps_after_the_slot_generation_wraps() {
    // Cycling a slot through more than one full generation range must not let the
    // original `$k1` match a live continuation again: the wrapped slot is retired.
    let wat = format!(
        r#"(module {PRELUDE}
          (func (export "run") (result i32)
            (local $k1 (ref null $ct)) (local $k (ref null $ct)) (local $n i32)
            (local.set $k1 (cont.new $ct (ref.func $gen)))
            (block $y (result i32 (ref $ct))
              (resume $ct (on $yield $y) (local.get $k1))
              unreachable)
            local.set $k1
            drop
            (drop (resume $ct (local.get $k1)))
            (loop $again
              (local.set $k (cont.new $ct (ref.func $gen)))
              (block $y2 (result i32 (ref $ct))
                (resume $ct (on $yield $y2) (local.get $k))
                unreachable)
              local.set $k
              drop
              (drop (resume $ct (local.get $k)))
              (local.tee $n (i32.add (local.get $n) (i32.const 1)))
              i32.const 4096 i32.lt_s
              br_if $again)
            (local.set $k (cont.new $ct (ref.func $gen)))
            (resume $ct (local.get $k1))))"#
    );
    expect_trap_with(
        "cont_handles_epoch_wrap",
        &wat,
        "let mut inst = Instance::new(); inst.func1();",
        "consumed continuation",
    );
}
