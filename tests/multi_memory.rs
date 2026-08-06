//! End-to-end tests for the multi-memory proposal (a module with more than one
//! linear memory). Each memory is an independent byte buffer; loads, stores,
//! data segments, `memory.size`/`grow`, and bulk `memory.copy`/`fill` all carry
//! a memory index that selects which buffer they act on.

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
fn two_memories_are_independent() {
    // A store to memory 1 must not be visible in memory 0: the two buffers are
    // separate. `i32.store 1` / `i32.load 1` name memory 1; the bare forms name
    // memory 0.
    compile_run(
        "multimem_isolation",
        r#"(module
            (memory 1)
            (memory 1)
            (func (export "w1") (param i32)
              i32.const 0 local.get 0 i32.store 1)
            (func (export "r1") (result i32)
              i32.const 0 i32.load 1)
            (func (export "r0") (result i32)
              i32.const 0 i32.load))"#,
        "let mut inst = Instance::new(); \
         inst.func0(42); \
         assert_eq!(inst.func1(), 42); \
         assert_eq!(inst.func2(), 0);",
    );
}

#[test]
fn data_segments_target_their_memory() {
    // An active data segment initialises the memory named by its `(memory i)`
    // clause; the default clause targets memory 0.
    compile_run(
        "multimem_data",
        r#"(module
            (memory 1)
            (memory 1)
            (data (i32.const 0) "\01")
            (data (memory 1) (i32.const 0) "\2a")
            (func (export "r0") (result i32)
              i32.const 0 i32.load8_u)
            (func (export "r1") (result i32)
              i32.const 0 i32.load8_u 1))"#,
        "let mut inst = Instance::new(); \
         assert_eq!(inst.func0(), 1); \
         assert_eq!(inst.func1(), 42);",
    );
}

#[test]
fn memory_copy_moves_bytes_across_memories() {
    // `memory.copy dst src` copies from the source memory into the destination
    // memory. Copy four bytes from memory 0 into memory 1 and read them back.
    compile_run(
        "multimem_copy",
        r#"(module
            (memory 1)
            (memory 1)
            (data (i32.const 0) "\aa\bb\cc\dd")
            (func (export "copy_0_to_1")
              i32.const 0 i32.const 0 i32.const 4 memory.copy 1 0)
            (func (export "r1") (param i32) (result i32)
              local.get 0 i32.load8_u 1))"#,
        "let mut inst = Instance::new(); \
         inst.func0(); \
         assert_eq!(inst.func1(0), 0xaa); \
         assert_eq!(inst.func1(3), 0xdd); \
         assert_eq!(inst.func1(1), 0xbb);",
    );
}

#[test]
fn memory_fill_targets_its_memory() {
    // `memory.fill i` writes only memory `i`. Fill memory 1 and confirm memory 0
    // is untouched.
    compile_run(
        "multimem_fill",
        r#"(module
            (memory 1)
            (memory 1)
            (func (export "fill1")
              i32.const 0 i32.const 7 i32.const 4 memory.fill 1)
            (func (export "r1") (result i32)
              i32.const 0 i32.load8_u 1)
            (func (export "r0") (result i32)
              i32.const 0 i32.load8_u))"#,
        "let mut inst = Instance::new(); \
         inst.func0(); \
         assert_eq!(inst.func1(), 7); \
         assert_eq!(inst.func2(), 0);",
    );
}

#[test]
fn memory_size_and_grow_select_their_memory() {
    // `memory.size i` / `memory.grow i` act on memory `i`. Memory 0 is one page,
    // memory 1 starts at two pages; growing memory 1 by one returns its old size
    // (2) and leaves it at three, without touching memory 0.
    compile_run(
        "multimem_size_grow",
        r#"(module
            (memory 1)
            (memory 2 4)
            (func (export "sz0") (result i32) memory.size)
            (func (export "sz1") (result i32) memory.size 1)
            (func (export "grow1") (result i32)
              i32.const 1 memory.grow 1))"#,
        "let mut inst = Instance::new(); \
         assert_eq!(inst.func0(), 1); \
         assert_eq!(inst.func1(), 2); \
         assert_eq!(inst.func2(), 2); \
         assert_eq!(inst.func1(), 3); \
         assert_eq!(inst.func0(), 1);",
    );
}
