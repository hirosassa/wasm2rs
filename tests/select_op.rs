//! End-to-end tests for the `select` / typed `select (result t)` operators.
//!
//! `select` pops (a, b, cond) and yields `a` when `cond != 0`, else `b`. The
//! typed form additionally carries the result type. These pin that the correct
//! operand is chosen for both truthy and falsy conditions and that the value
//! type is preserved (i32 vs i64), which a bug swapping the arms or dropping the
//! type would break.

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
fn untyped_select_picks_first_when_condition_is_nonzero() {
    compile_run(
        "select_untyped",
        r#"(module
            (func (export "f") (param i32 i32 i32) (result i32)
              local.get 0 local.get 1 local.get 2 select))"#,
        "assert_eq!(func0(10, 20, 1), 10); \
         assert_eq!(func0(10, 20, 5), 10); \
         assert_eq!(func0(10, 20, 0), 20);",
    );
}

#[test]
fn typed_select_preserves_the_result_type() {
    compile_run(
        "select_typed",
        r#"(module
            (func (export "f") (param i64 i64 i32) (result i64)
              local.get 0 local.get 1 local.get 2 (select (result i64))))"#,
        "assert_eq!(func0(0x0102_0304_0506_0708, 0x0900_0000_0000_0000, 1), \
                    0x0102_0304_0506_0708); \
         assert_eq!(func0(0x0102_0304_0506_0708, 0x0900_0000_0000_0000, 0), \
                    0x0900_0000_0000_0000);",
    );
}
