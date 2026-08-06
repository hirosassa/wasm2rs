//! A rotate (`i32.rotl`/`i32.rotr`/`i64.rotl`/`i64.rotr`) whose left operand is a
//! negative constant must lower with the receiver parenthesised. Emitting the
//! bare literal — `-2i32.rotate_left(n)` — mis-parses in Rust, because a method
//! call binds tighter than unary minus, so it becomes `-(2i32.rotate_left(n))`
//! rather than `(-2i32).rotate_left(n)`. Rotation does not commute with negation,
//! so the two differ. This is exactly how dlmalloc's `clear_treemap`/
//! `clear_smallmap` masks (`map & rotl(-2, idx)`) get corrupted, which silently
//! wrecks the allocator's bin maps.

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

#[test]
fn rotate_with_negative_constant_receiver() {
    let wat = r#"
        (module
          ;; func0: i32.rotl(-2, n)
          (func (param i32) (result i32)
            i32.const -2
            local.get 0
            i32.rotl)
          ;; func1: i32.rotr(-2, n)
          (func (param i32) (result i32)
            i32.const -2
            local.get 0
            i32.rotr)
          ;; func2: i64.rotl(-2, n)
          (func (param i64) (result i64)
            i64.const -2
            local.get 0
            i64.rotl)
          ;; func3: the dlmalloc clear_treemap idiom: map & rotl(-2, idx)
          (func (param i32 i32) (result i32)
            local.get 0
            i32.const -2
            local.get 1
            i32.rotl
            i32.and))
    "#;

    let main_body = r#"
        // (-2) rotl 9 = 0xFFFFFFFE rotl 9 = 0xFFFFFDFF (the single clear bit
        // lands at position 9). The buggy lowering yields -(2 rotl 9) = -1024.
        assert_eq!(func0(9), 0xFFFFFDFFu32 as i32, "i32.rotl(-2, 9)");
        assert_eq!(func0(0), -2, "i32.rotl(-2, 0)");

        // (-2) rotr 1 = 0x7FFFFFFF.
        assert_eq!(func1(1), 0x7FFFFFFFi32, "i32.rotr(-2, 1)");

        // (-2i64) rotl 9 = 0xFFFFFFFFFFFFFDFF = -513.
        assert_eq!(func2(9), -513i64, "i64.rotl(-2, 9)");

        // clear_treemap: map 0xb00 (bits 8,9,11), clear bit 9 -> 0x900.
        // The buggy mask 0xFFFFFC00 would over-clear to 0x800.
        assert_eq!(func3(0xb00, 9), 0x900, "map & rotl(-2, 9)");
    "#;

    common::compile_run("rotate_neg_receiver", wat, main_body);
}
