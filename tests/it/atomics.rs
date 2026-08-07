//! End-to-end tests for the threads/atomics proposal.
//!
//! The generated `Instance` owns its linear memory exclusively (`&mut self`),
//! so within one instance every atomic operation is trivially atomic: it lowers
//! to an ordinary load / read-modify-write / store. True cross-thread shared
//! memory is out of scope (see WASM2GO_COMPARISON.md §E-3); these tests pin the
//! single-instance semantics.

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
fn atomic_rmw_arithmetic_and_bitwise_ops_i32() {
    // Each RMW op returns the old value and stores `combine(old, operand)`. This
    // pins the opcode->RmwOp dispatch for sub/or/xor (add/xchg are covered above),
    // where a copy-paste in the match arm would compute the wrong combine. Each
    // func returns old + final_memory so both halves are checked at once.
    compile_run(
        "atomic_rmw_ops_i32",
        r#"(module
            (memory 1)
            (func (export "sub") (result i32)
              i32.const 0 i32.const 20 i32.atomic.store
              i32.const 0 i32.const 5  i32.atomic.rmw.sub   ;; old 20, mem := 15
              i32.const 0 i32.atomic.load i32.add)           ;; 20 + 15 = 35
            (func (export "or") (result i32)
              i32.const 0 i32.const 0x0A i32.atomic.store
              i32.const 0 i32.const 0x05 i32.atomic.rmw.or   ;; old 10, mem := 15
              i32.const 0 i32.atomic.load i32.add)           ;; 10 + 15 = 25
            (func (export "xor") (result i32)
              i32.const 0 i32.const 0x0C i32.atomic.store
              i32.const 0 i32.const 0x0A i32.atomic.rmw.xor  ;; old 12, mem := 6
              i32.const 0 i32.atomic.load i32.add))          ;; 12 + 6 = 18
        "#,
        "let mut inst = Instance::new(); \
         assert_eq!(inst.func0(), 35); \
         assert_eq!(inst.func1(), 25); \
         assert_eq!(inst.func2(), 18);",
    );
}

#[test]
fn atomic_rmw_and_cmpxchg_i64_full_width() {
    // The i64 RMW/cmpxchg path (AtomicWidth::I64) is otherwise untested: only i32
    // widths appear above. `and` combines full width; `cmpxchg` swaps only on a
    // matching i64 value and returns the old one either way.
    compile_run(
        "atomic_i64_full",
        r#"(module
            (memory 1)
            (func (export "and") (result i64)
              i32.const 0 i64.const 0xF0F0 i64.atomic.store
              i32.const 0 i64.const 0xFF00 i64.atomic.rmw.and ;; old 0xF0F0, mem := 0xF000
              i32.const 0 i64.atomic.load i64.add)             ;; 0xF0F0 + 0xF000
            (func (export "cx") (result i64)
              i32.const 0 i64.const 10 i64.atomic.store
              i32.const 0 i64.const 10 i64.const 99 i64.atomic.rmw.cmpxchg ;; match: old 10, mem := 99
              i32.const 0 i64.const 7  i64.const 42 i64.atomic.rmw.cmpxchg ;; miss: old 99, mem stays 99
              i64.add                                                       ;; 10 + 99 = 109
              i32.const 0 i64.atomic.load i64.add))                         ;; 109 + 99 = 208
        "#,
        "let mut inst = Instance::new(); \
         assert_eq!(inst.func0(), 0xF0F0i64 + 0xF000i64); \
         assert_eq!(inst.func1(), 208);",
    );
}

#[test]
fn atomic_rmw_narrow_widths_truncate_on_store() {
    // A narrow RMW combines at full width but the store keeps only the low
    // 8/16/32 bits, while the returned old value is the zero-extended cell. This
    // exercises every narrow AtomicWidth (I32As16, I64As8/16/32) and their
    // zero-extending loads / truncating stores.
    compile_run(
        "atomic_rmw_narrow",
        r#"(module
            (memory 1)
            (func (export "i32_16") (result i32)
              i32.const 0 i32.const 0xFFFF i32.atomic.store16
              i32.const 0 i32.const 2 i32.atomic.rmw16.add_u ;; old 65535, mem16 := 65537 & 0xFFFF = 1
              i32.const 0 i32.atomic.load16_u i32.add)        ;; 65535 + 1 = 65536
            (func (export "i64_8") (result i64)
              i32.const 0 i64.const 0xFF i64.atomic.store8
              i32.const 0 i64.const 3 i64.atomic.rmw8.add_u  ;; old 255, mem8 := 258 & 0xFF = 2
              i32.const 0 i64.atomic.load8_u i64.add)         ;; 255 + 2 = 257
            (func (export "i64_16") (result i64)
              i32.const 0 i64.const 0xFFFF i64.atomic.store16
              i32.const 0 i64.const 2 i64.atomic.rmw16.add_u ;; old 65535, mem16 := 1
              i32.const 0 i64.atomic.load16_u i64.add)        ;; 65536
            (func (export "i64_32") (result i64)
              i32.const 0 i64.const 0xFFFF_FFFF i64.atomic.store32
              i32.const 0 i64.const 2 i64.atomic.rmw32.add_u ;; old 4294967295, mem32 := 1
              i32.const 0 i64.atomic.load32_u i64.add))       ;; 4294967295 + 1
        "#,
        "let mut inst = Instance::new(); \
         assert_eq!(inst.func0(), 65536); \
         assert_eq!(inst.func1(), 257); \
         assert_eq!(inst.func2(), 65536); \
         assert_eq!(inst.func3(), 0xFFFF_FFFFi64 + 1);",
    );
}

#[test]
fn atomic_cmpxchg_narrow_compares_at_access_width() {
    // A narrow cmpxchg masks `expected` to the access width before comparing, so
    // high bits beyond the cell are ignored (spec: compare at the access width).
    // Passing 0x1AB with a byte cell of 0xAB must still match; a bug that compared
    // unmasked (0x1AB != 0xAB) would skip the swap and leave 0xAB.
    compile_run(
        "atomic_cmpxchg_narrow",
        r#"(module
            (memory 1)
            (func (export "byte") (result i32)
              i32.const 0 i32.const 0xAB i32.atomic.store8
              i32.const 0 i32.const 0x1AB i32.const 0xCD i32.atomic.rmw8.cmpxchg_u ;; (0x1AB & 0xFF)=0xAB matches
              i32.const 0 i32.atomic.load8_u i32.add)  ;; old 0xAB + new 0xCD
            (func (export "i64_32") (result i64)
              i32.const 0 i64.const 0xDEAD_BEEF i64.atomic.store32
              i32.const 0 i64.const 0xDEAD_BEEF i64.const 0x1234_5678 i64.atomic.rmw32.cmpxchg_u
              i32.const 0 i64.atomic.load32_u i64.add)) ;; old 0xDEADBEEF + new 0x12345678
        "#,
        "let mut inst = Instance::new(); \
         assert_eq!(inst.func0(), 0xAB + 0xCD); \
         assert_eq!(inst.func1(), 0xDEAD_BEEFi64 + 0x1234_5678i64);",
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
