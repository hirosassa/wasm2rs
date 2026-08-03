//! End-to-end tests for the threads/atomics proposal.
//!
//! The generated `Instance` owns its linear memory exclusively (`&mut self`),
//! so within one instance every atomic operation is trivially atomic: it lowers
//! to an ordinary load / read-modify-write / store. True cross-thread shared
//! memory is out of scope (see WASM2GO_COMPARISON.md §E-3); these tests pin the
//! single-instance semantics.

mod common;

use common::{compile_run, expect_trap_with};

#[test]
fn atomic_store_then_load_roundtrips() {
    // `i32.atomic.store` writes a value that `i32.atomic.load` reads back — the
    // atomic ops lower to the same little-endian memory access as the plain ones.
    compile_run(
        "atomic_load_store",
        r#"(module
            (memory 1)
            (func (export "f") (param i32) (result i32)
              i32.const 0
              local.get 0
              i32.atomic.store
              i32.const 0
              i32.atomic.load))"#,
        "let mut inst = Instance::new(); \
         assert_eq!(inst.func0(42), 42); assert_eq!(inst.func0(-7), -7);",
    );
}

#[test]
fn atomic_narrow_and_i64_accesses() {
    // A narrow atomic store keeps only the low byte, and its zero-extending
    // atomic load reads it back; the i64 access round-trips a full 64-bit value.
    compile_run(
        "atomic_narrow_i64",
        r#"(module
            (memory 1)
            (func (export "byte") (param i32) (result i32)
              i32.const 4
              local.get 0
              i32.atomic.store8
              i32.const 4
              i32.atomic.load8_u)
            (func (export "wide") (param i64) (result i64)
              i32.const 8
              local.get 0
              i64.atomic.store
              i32.const 8
              i64.atomic.load))"#,
        "let mut inst = Instance::new(); \
         assert_eq!(inst.func0(0x1FF), 0xFF); \
         assert_eq!(inst.func1(0x0102_0304_0506_0708u64 as i64), 0x0102_0304_0506_0708u64 as i64);",
    );
}

#[test]
fn atomic_rmw_add_returns_old_and_updates_memory() {
    // `i32.atomic.rmw.add` reads the current value (returned) and writes back the
    // sum: seed 10, add 5 → returns 10, and a later load sees 15 (10 + 15 = 25).
    compile_run(
        "atomic_rmw_add",
        r#"(module
            (memory 1)
            (func (export "f") (result i32)
              i32.const 0
              i32.const 10
              i32.atomic.store
              i32.const 0
              i32.const 5
              i32.atomic.rmw.add
              i32.const 0
              i32.atomic.load
              i32.add))"#,
        "let mut inst = Instance::new(); assert_eq!(inst.func0(), 25);",
    );
}

#[test]
fn atomic_rmw_xchg_and_narrow_truncate() {
    // `xchg` swaps in the operand and returns the old value; a narrow `and_u`
    // combines at full width but the store keeps only the low byte, while the
    // returned old value is the zero-extended byte.
    compile_run(
        "atomic_rmw_xchg_narrow",
        r#"(module
            (memory 1)
            (func (export "x") (result i32)
              i32.const 0
              i32.const 100
              i32.atomic.store
              i32.const 0
              i32.const 7
              i32.atomic.rmw.xchg      ;; returns 100, mem[0] := 7
              i32.const 0
              i32.atomic.load
              i32.add)                 ;; 100 + 7 = 107
            (func (export "n") (result i32)
              i32.const 0
              i32.const 0xFF
              i32.atomic.store8
              i32.const 0
              i32.const 0x0F
              i32.atomic.rmw8.and_u    ;; returns 255, mem[0] := 0x0F
              i32.const 0
              i32.atomic.load8_u
              i32.add))                ;; 255 + 15 = 270
        "#,
        "let mut inst = Instance::new(); \
         assert_eq!(inst.func0(), 107); assert_eq!(inst.func1(), 270);",
    );
}

#[test]
fn atomic_cmpxchg_swaps_only_on_match() {
    // `cmpxchg` stores the replacement only when the current value equals the
    // expected one; either way it returns the old value. First call matches
    // (10 → 99), second does not (expected 5 ≠ 99), so memory stays 99.
    compile_run(
        "atomic_cmpxchg",
        r#"(module
            (memory 1)
            (func (export "f") (result i32)
              i32.const 0
              i32.const 10
              i32.atomic.store
              i32.const 0
              i32.const 10
              i32.const 99
              i32.atomic.rmw.cmpxchg   ;; expected 10 == 10 → old 10, mem := 99
              i32.const 0
              i32.const 5
              i32.const 42
              i32.atomic.rmw.cmpxchg   ;; expected 5 != 99 → old 99, mem stays 99
              i32.add                  ;; 10 + 99 = 109
              i32.const 0
              i32.atomic.load
              i32.add))                ;; 109 + 99 = 208
        "#,
        "let mut inst = Instance::new(); assert_eq!(inst.func0(), 208);",
    );
}

#[test]
fn atomic_fence_notify_and_wait_mismatch() {
    // `atomic.fence` is a no-op on a single instance; `memory.atomic.notify`
    // wakes nobody (returns 0); `memory.atomic.wait32` returns 1 ("not equal")
    // when the cell (0) differs from the expected value.
    compile_run(
        "atomic_fence_notify_wait",
        r#"(module
            (memory 1)
            (func (export "n") (result i32)
              atomic.fence
              i32.const 0
              i32.const 1
              memory.atomic.notify)
            (func (export "w") (result i32)
              i32.const 0
              i32.const 5
              i64.const -1
              memory.atomic.wait32))"#,
        "let mut inst = Instance::new(); \
         assert_eq!(inst.func0(), 0); assert_eq!(inst.func1(), 1);",
    );
}

#[test]
fn atomic_wait_on_match_traps() {
    // With no other thread able to wake it, a `wait` whose expected value equals
    // the cell would block forever, so a single-threaded instance traps instead.
    expect_trap_with(
        "atomic_wait_block",
        r#"(module
            (memory 1)
            (func (export "t")
              i32.const 0
              i32.const 0
              i64.const -1
              memory.atomic.wait32
              drop))"#,
        "let mut inst = Instance::new(); inst.func0();",
        "would block forever",
    );
}

#[test]
fn shared_memory_is_accepted() {
    // A `shared` memory (the threads proposal) is accepted and transpiles like an
    // ordinary one: the single owning instance makes its atomics trivially safe.
    // Real cross-thread sharing is out of scope (see WASM2GO_COMPARISON.md §E-3).
    compile_run(
        "atomic_shared_mem",
        r#"(module
            (memory 1 1 shared)
            (func (export "f") (param i32) (result i32)
              i32.const 0
              local.get 0
              i32.atomic.store
              i32.const 0
              i32.atomic.load))"#,
        "let mut inst = Instance::new(); assert_eq!(inst.func0(42), 42);",
    );
}
