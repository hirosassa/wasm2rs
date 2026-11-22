//! Integration tests for Phase 6a: i64 numeric operators (const, wrapping
//! arithmetic, bitwise, and signed/unsigned comparisons). These modules are
//! stateless, so the transpiler emits free functions; each is compiled with
//! `rustc -D warnings` and exercised.

use crate::common::compile_run;

#[test]
fn i64_arithmetic_and_bitwise_wrap() {
    compile_run(
        "arith",
        r#"
        (module
          (func (param i64 i64) (result i64) (i64.add (local.get 0) (local.get 1)))
          (func (param i64 i64) (result i64) (i64.sub (local.get 0) (local.get 1)))
          (func (param i64 i64) (result i64) (i64.mul (local.get 0) (local.get 1)))
          (func (param i64 i64) (result i64) (i64.and (local.get 0) (local.get 1)))
          (func (param i64 i64) (result i64) (i64.or (local.get 0) (local.get 1)))
          (func (param i64 i64) (result i64) (i64.xor (local.get 0) (local.get 1))))
        "#,
        "assert_eq!(func0(2, 3), 5);\n    \
         assert_eq!(func0(i64::MAX, 1), i64::MIN);\n    \
         assert_eq!(func1(3, 5), -2);\n    \
         assert_eq!(func2(6, 7), 42);\n    \
         assert_eq!(func3(0b1100, 0b1010), 0b1000);\n    \
         assert_eq!(func4(0b1100, 0b1010), 0b1110);\n    \
         assert_eq!(func5(0b1100, 0b1010), 0b0110);",
    );
}

#[test]
fn i64_comparisons_are_signed_and_unsigned() {
    compile_run(
        "cmp",
        r#"
        (module
          (func (param i64) (result i32) (i64.eqz (local.get 0)))
          (func (param i64 i64) (result i32) (i64.eq (local.get 0) (local.get 1)))
          (func (param i64 i64) (result i32) (i64.lt_s (local.get 0) (local.get 1)))
          (func (param i64 i64) (result i32) (i64.lt_u (local.get 0) (local.get 1)))
          (func (param i64 i64) (result i32) (i64.ge_u (local.get 0) (local.get 1))))
        "#,
        "assert_eq!(func0(0), 1);\n    \
         assert_eq!(func0(9), 0);\n    \
         assert_eq!(func1(7, 7), 1);\n    \
         assert_eq!(func2(-1, 0), 1);\n    \
         assert_eq!(func3(-1, 0), 0);\n    \
         assert_eq!(func4(-1, 0), 1);",
    );
}

#[test]
fn i64_shifts_rotates_and_remaining_comparisons() {
    // Shift/rotate counts are masked mod 64, and the signed/unsigned split shows
    // up in rem_u and the gt/le/ge comparisons when the operands differ in sign.
    compile_run(
        "shifts",
        r#"
        (module
          (func (param i64 i64) (result i64) (i64.shl (local.get 0) (local.get 1)))
          (func (param i64 i64) (result i64) (i64.shr_s (local.get 0) (local.get 1)))
          (func (param i64 i64) (result i64) (i64.rotr (local.get 0) (local.get 1)))
          (func (param i64 i64) (result i64) (i64.rem_u (local.get 0) (local.get 1)))
          (func (param i64 i64) (result i32) (i64.ne (local.get 0) (local.get 1)))
          (func (param i64 i64) (result i32) (i64.gt_s (local.get 0) (local.get 1)))
          (func (param i64 i64) (result i32) (i64.gt_u (local.get 0) (local.get 1)))
          (func (param i64 i64) (result i32) (i64.le_s (local.get 0) (local.get 1)))
          (func (param i64 i64) (result i32) (i64.le_u (local.get 0) (local.get 1)))
          (func (param i64 i64) (result i32) (i64.ge_s (local.get 0) (local.get 1))))
        "#,
        "assert_eq!(func0(3, 64), 3);\n    \
         assert_eq!(func0(1, 1), 2);\n    \
         assert_eq!(func1(-4, 65), -2);\n    \
         assert_eq!(func2(0x0123_4567_89AB_CDEF, 8), 0xEF01_2345_6789_ABCDu64 as i64);\n    \
         assert_eq!(func3(-1, 5), 0);\n    \
         assert_eq!(func4(7, 7), 0);\n    \
         assert_eq!(func4(7, 8), 1);\n    \
         assert_eq!(func5(-1, 0), 0);\n    \
         assert_eq!(func6(-1, 0), 1);\n    \
         assert_eq!(func7(-1, 0), 1);\n    \
         assert_eq!(func8(-1, 0), 0);\n    \
         assert_eq!(func9(0, -1), 1);",
    );
}
