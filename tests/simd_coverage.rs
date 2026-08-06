//! Additional E2E coverage tests for SIMD/v128 operators not exercised by
//! tests/simd.rs. Each test transpiles a WAT module, compiles the generated
//! Rust with `rustc -D warnings`, runs it, and asserts exact lane values.

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

use std::process::Command;

fn compile(test: &str, wat: &str, main_body: &str) -> std::path::PathBuf {
    let wasm = wat::parse_str(wat).expect("valid wat");
    let generated = wasm2rs::transpile(&wasm).expect("transpile ok");

    let dir = std::env::temp_dir().join(format!("wasm2rs_simd_cov_{test}"));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let src = dir.join("gen.rs");
    let bin = dir.join(if cfg!(windows) { "gen.exe" } else { "gen" });

    let program = format!("{generated}\nfn main() {{\n{main_body}\n}}\n");
    std::fs::write(&src, &program).expect("write generated source");

    let out = Command::new("rustc")
        .current_dir(&dir)
        .arg(&src)
        .arg("--edition")
        .arg("2021")
        .arg("-D")
        .arg("warnings")
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("run rustc");
    assert!(
        out.status.success(),
        "generated code failed to compile:\n{program}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&out.stderr),
    );
    bin
}

fn expect_ok(test: &str, wat: &str, main_body: &str) {
    let bin = compile(test, wat, main_body);
    let run = Command::new(&bin).status().expect("run generated binary");
    assert!(run.success(), "generated program did not succeed: {test}");
}

// ---------------------------------------------------------------------------
// replace_lane for i8x16, i16x8, i64x2, f32x4
// ---------------------------------------------------------------------------

#[test]
fn replace_lane_i8x16_i16x8_i64x2_f32x4() {
    // Build a vector, replace one lane, then extract the replaced lane and an
    // untouched lane to prove only the target changed.
    expect_ok(
        "replace_lane_variants",
        r#"
        (module
          ;; i8x16: replace lane 3 with 99; check lane 3 (replaced) and lane 0 (untouched)
          (func (result i32)
            (i8x16.extract_lane_s 3
              (i8x16.replace_lane 3 (v128.const i8x16 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16)
                                    (i32.const 99))))
          (func (result i32)
            (i8x16.extract_lane_s 0
              (i8x16.replace_lane 3 (v128.const i8x16 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16)
                                    (i32.const 99))))
          ;; i16x8: replace lane 5 with -300 (sign-extends when extracted as _s)
          (func (result i32)
            (i16x8.extract_lane_s 5
              (i16x8.replace_lane 5 (v128.const i16x8 0 0 0 0 0 0 0 0)
                                    (i32.const -300))))
          (func (result i32)
            (i16x8.extract_lane_s 2
              (i16x8.replace_lane 5 (v128.const i16x8 0 0 77 0 0 0 0 0)
                                    (i32.const -300))))
          ;; i64x2: replace lane 1 with a large value
          (func (result i64)
            (i64x2.extract_lane 1
              (i64x2.replace_lane 1 (v128.const i64x2 111 222)
                                    (i64.const 999999999999))))
          (func (result i64)
            (i64x2.extract_lane 0
              (i64x2.replace_lane 1 (v128.const i64x2 111 222)
                                    (i64.const 999999999999))))
          ;; f32x4: replace lane 2 with 3.5
          (func (result f32)
            (f32x4.extract_lane 2
              (f32x4.replace_lane 2 (v128.const f32x4 1.0 2.0 3.0 4.0)
                                    (f32.const 3.5))))
          (func (result f32)
            (f32x4.extract_lane 1
              (f32x4.replace_lane 2 (v128.const f32x4 1.0 2.0 3.0 4.0)
                                    (f32.const 3.5)))))
        "#,
        "assert_eq!(func0(), 99);\n    \
         assert_eq!(func1(), 1);\n    \
         assert_eq!(func2(), -300);\n    \
         assert_eq!(func3(), 77);\n    \
         assert_eq!(func4(), 999999999999i64);\n    \
         assert_eq!(func5(), 111i64);\n    \
         assert_eq!(func6(), 3.5f32);\n    \
         assert_eq!(func7(), 2.0f32);",
    );
}

// ---------------------------------------------------------------------------
// Integer arith: i16x8.add, i32x4.add, i8x16.sub, i32x4.sub, i64x2.sub,
//                i16x8.mul, i64x2.mul
// ---------------------------------------------------------------------------

#[test]
fn integer_arith_binops() {
    // Wrapping arithmetic. Choose values that wrap to prove wrapping (not clamping).
    expect_ok(
        "int_arith_binops",
        r#"
        (module
          ;; i16x8.add: 32767 + 1 wraps to -32768 (i16 wrap)
          (func (result i32)
            (i16x8.extract_lane_s 0
              (i16x8.add (v128.const i16x8 32767 0 0 0 0 0 0 0)
                         (v128.const i16x8 1     0 0 0 0 0 0 0))))
          ;; i32x4.add: large positive + 1
          (func (result i32)
            (i32x4.extract_lane 1
              (i32x4.add (v128.const i32x4 0 2000000000 0 0)
                         (v128.const i32x4 0 500000000  0 0))))
          ;; i8x16.sub: 3 - 10 = -7
          (func (result i32)
            (i8x16.extract_lane_s 0
              (i8x16.sub (v128.const i8x16 3 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0)
                         (v128.const i8x16 10 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0))))
          ;; i32x4.sub: large values
          (func (result i32)
            (i32x4.extract_lane 2
              (i32x4.sub (v128.const i32x4 0 0 1000000 0)
                         (v128.const i32x4 0 0 1        0))))
          ;; i64x2.sub
          (func (result i64)
            (i64x2.extract_lane 0
              (i64x2.sub (v128.const i64x2 1000000000000 0)
                         (v128.const i64x2 999999999999  0))))
          ;; i16x8.mul: 256 * 256 = 65536 wraps to 0 in i16
          (func (result i32)
            (i16x8.extract_lane_s 0
              (i16x8.mul (v128.const i16x8 256 0 0 0 0 0 0 0)
                         (v128.const i16x8 256 0 0 0 0 0 0 0))))
          ;; i64x2.mul
          (func (result i64)
            (i64x2.extract_lane 1
              (i64x2.mul (v128.const i64x2 0 1000000)
                         (v128.const i64x2 0 1000000)))))
        "#,
        // 32767+1 wraps to -32768; 2e9+5e8=2.5e9 (fits i32); 3-10=-7; 999999;
        // 1; 256*256=65536 wraps i16 to 0; 1e12 (i64).
        "assert_eq!(func0(), -32768);\n    \
         assert_eq!(func1(), 2500000000u32 as i32);\n    \
         assert_eq!(func2(), -7);\n    \
         assert_eq!(func3(), 999999);\n    \
         assert_eq!(func4(), 1i64);\n    \
         assert_eq!(func5(), 0);\n    \
         assert_eq!(func6(), 1_000_000_000_000i64);",
    );
}

// ---------------------------------------------------------------------------
// Integer neg: i16x8.neg, i64x2.neg
// ---------------------------------------------------------------------------

#[test]
fn integer_neg_i16x8_i64x2() {
    expect_ok(
        "int_neg_i16_i64",
        r#"
        (module
          ;; i16x8.neg: negate lane 3
          (func (result i32)
            (i16x8.extract_lane_s 3
              (i16x8.neg (v128.const i16x8 0 0 0 -500 0 0 0 0))))
          ;; i16x8.neg: i16::MIN stays i16::MIN (wrapping_neg)
          (func (result i32)
            (i16x8.extract_lane_s 0
              (i16x8.neg (v128.const i16x8 -32768 0 0 0 0 0 0 0))))
          ;; i64x2.neg: negate a large value
          (func (result i64)
            (i64x2.extract_lane 1
              (i64x2.neg (v128.const i64x2 0 123456789012)))))
        "#,
        "assert_eq!(func0(), 500);\n    \
         assert_eq!(func1(), -32768);\n    \
         assert_eq!(func2(), -123456789012i64);",
    );
}

// ---------------------------------------------------------------------------
// Float binops: f64x2.sub, f64x2.mul, f32x4.div, f64x2.min, f64x2.max
// ---------------------------------------------------------------------------

#[test]
fn float_binops_f64x2_f32x4_div() {
    expect_ok(
        "float_binops_extra",
        r#"
        (module
          ;; f64x2.sub
          (func (result f64)
            (f64x2.extract_lane 0
              (f64x2.sub (v128.const f64x2 10.0 0) (v128.const f64x2 3.5 0))))
          ;; f64x2.mul
          (func (result f64)
            (f64x2.extract_lane 1
              (f64x2.mul (v128.const f64x2 0 4.0) (v128.const f64x2 0 2.5))))
          ;; f32x4.div: 15.0 / 3.0 = 5.0
          (func (result f32)
            (f32x4.extract_lane 0
              (f32x4.div (v128.const f32x4 15.0 0 0 0) (v128.const f32x4 3.0 0 0 0))))
          ;; f64x2.min: 1.0 vs 2.0 -> 1.0
          (func (result f64)
            (f64x2.extract_lane 0
              (f64x2.min (v128.const f64x2 1.0 5.0) (v128.const f64x2 2.0 3.0))))
          ;; f64x2.max: 1.0 vs 2.0 -> 2.0
          (func (result f64)
            (f64x2.extract_lane 1
              (f64x2.max (v128.const f64x2 1.0 5.0) (v128.const f64x2 2.0 3.0)))))
        "#,
        "assert_eq!(func0(), 6.5f64);\n    \
         assert_eq!(func1(), 10.0f64);\n    \
         assert_eq!(func2(), 5.0f32);\n    \
         assert_eq!(func3(), 1.0f64);\n    \
         assert_eq!(func4(), 5.0f64);",
    );
}

// ---------------------------------------------------------------------------
// Float unops: f64x2.ceil, f64x2.floor, f64x2.trunc, f32x4.nearest
// ---------------------------------------------------------------------------

#[test]
fn float_unops_rounding() {
    expect_ok(
        "float_unops_rounding",
        r#"
        (module
          ;; f64x2.ceil: 1.3 -> 2.0
          (func (result f64)
            (f64x2.extract_lane 0
              (f64x2.ceil (v128.const f64x2 1.3 -2.7))))
          ;; f64x2.ceil: -2.7 -> -2.0
          (func (result f64)
            (f64x2.extract_lane 1
              (f64x2.ceil (v128.const f64x2 1.3 -2.7))))
          ;; f64x2.floor: 2.9 -> 2.0
          (func (result f64)
            (f64x2.extract_lane 0
              (f64x2.floor (v128.const f64x2 2.9 -0.1))))
          ;; f64x2.floor: -0.1 -> -1.0
          (func (result f64)
            (f64x2.extract_lane 1
              (f64x2.floor (v128.const f64x2 2.9 -0.1))))
          ;; f64x2.trunc: -3.9 -> -3.0
          (func (result f64)
            (f64x2.extract_lane 0
              (f64x2.trunc (v128.const f64x2 -3.9 4.8))))
          ;; f32x4.nearest: 0.5 -> 0.0 (round half to even), 1.5 -> 2.0
          (func (result f32)
            (f32x4.extract_lane 0
              (f32x4.nearest (v128.const f32x4 0.5 1.5 0 0))))
          (func (result f32)
            (f32x4.extract_lane 1
              (f32x4.nearest (v128.const f32x4 0.5 1.5 0 0)))))
        "#,
        "assert_eq!(func0(), 2.0f64);\n    \
         assert_eq!(func1(), -2.0f64);\n    \
         assert_eq!(func2(), 2.0f64);\n    \
         assert_eq!(func3(), -1.0f64);\n    \
         assert_eq!(func4(), -3.0f64);\n    \
         assert_eq!(func5(), 0.0f32);\n    \
         assert_eq!(func6(), 2.0f32);",
    );
}

// ---------------------------------------------------------------------------
// Comparisons: i8x16.eq, i8x16.gt_s, i8x16.le_s, i8x16.ge_s
// ---------------------------------------------------------------------------

#[test]
fn compare_i8x16() {
    // Use a negative i8 lane to distinguish signed ops from unsigned ones.
    // Result is all-ones (0xFF = -1 when read as i8 via extract_lane_s) or 0.
    expect_ok(
        "compare_i8x16",
        r#"
        (module
          ;; i8x16.eq: 5 == 5 -> -1; 5 == 6 -> 0
          (func (result i32)
            (i8x16.extract_lane_s 0
              (i8x16.eq (i8x16.splat (i32.const 5)) (i8x16.splat (i32.const 5)))))
          (func (result i32)
            (i8x16.extract_lane_s 0
              (i8x16.eq (i8x16.splat (i32.const 5)) (i8x16.splat (i32.const 6)))))
          ;; i8x16.gt_s: signed -1 > 1 is false; but unsigned 0xFF > 1 is true
          ;; use gt_s and prove it returns 0 (false) for -1 > 1 signed
          (func (result i32)
            (i8x16.extract_lane_s 0
              (i8x16.gt_s (i8x16.splat (i32.const 0xFF)) (i8x16.splat (i32.const 1)))))
          ;; i8x16.le_s: -1 <= 1 signed is true -> -1
          (func (result i32)
            (i8x16.extract_lane_s 0
              (i8x16.le_s (i8x16.splat (i32.const 0xFF)) (i8x16.splat (i32.const 1)))))
          ;; i8x16.ge_s: 10 >= 10 -> -1
          (func (result i32)
            (i8x16.extract_lane_s 0
              (i8x16.ge_s (i8x16.splat (i32.const 10)) (i8x16.splat (i32.const 10))))))
        "#,
        "assert_eq!(func0(), -1);\n    \
         assert_eq!(func1(), 0);\n    \
         assert_eq!(func2(), 0);\n    \
         assert_eq!(func3(), -1);\n    \
         assert_eq!(func4(), -1);",
    );
}

// ---------------------------------------------------------------------------
// Comparisons: i16x8.eq, i16x8.lt_s, i16x8.gt_s, i16x8.le_s, i16x8.ge_u
// ---------------------------------------------------------------------------

#[test]
fn compare_i16x8() {
    // ge_u: use a negative lane (0x8000 = 32768 unsigned, -32768 signed).
    // Unsigned 0x8000 >= 1 is true; signed -32768 >= 1 is false.
    expect_ok(
        "compare_i16x8",
        r#"
        (module
          ;; i16x8.eq: 100 == 100 -> -1
          (func (result i32)
            (i16x8.extract_lane_s 0
              (i16x8.eq (v128.const i16x8 100 0 0 0 0 0 0 0)
                        (v128.const i16x8 100 0 0 0 0 0 0 0))))
          ;; i16x8.lt_s: signed -1 < 0 is true
          (func (result i32)
            (i16x8.extract_lane_s 0
              (i16x8.lt_s (v128.const i16x8 -1 0 0 0 0 0 0 0)
                          (v128.const i16x8  0 0 0 0 0 0 0 0))))
          ;; i16x8.gt_s: signed 1 > 0 -> -1
          (func (result i32)
            (i16x8.extract_lane_s 0
              (i16x8.gt_s (v128.const i16x8 1 0 0 0 0 0 0 0)
                          (v128.const i16x8 0 0 0 0 0 0 0 0))))
          ;; i16x8.le_s: 5 <= 5 -> -1
          (func (result i32)
            (i16x8.extract_lane_s 2
              (i16x8.le_s (v128.const i16x8 0 0 5 0 0 0 0 0)
                          (v128.const i16x8 0 0 5 0 0 0 0 0))))
          ;; i16x8.ge_u: 0x8000 unsigned >= 1 is true; signed -32768 >= 1 is false
          (func (result i32)
            (i16x8.extract_lane_s 0
              (i16x8.ge_u (v128.const i16x8 -32768 0 0 0 0 0 0 0)
                          (v128.const i16x8 1      0 0 0 0 0 0 0)))))
        "#,
        "assert_eq!(func0(), -1);\n    \
         assert_eq!(func1(), -1);\n    \
         assert_eq!(func2(), -1);\n    \
         assert_eq!(func3(), -1);\n    \
         assert_eq!(func4(), -1);",
    );
}

// ---------------------------------------------------------------------------
// Comparisons: i32x4.ne, i32x4.lt_u, i32x4.gt_u, i32x4.le_u, i32x4.ge_u
// ---------------------------------------------------------------------------

#[test]
fn compare_i32x4_unsigned() {
    // lt_u/gt_u/le_u/ge_u: use a value with the top bit set (-1 signed = 0xFFFF_FFFF
    // unsigned) to show the unsigned interpretation differs from signed.
    expect_ok(
        "compare_i32x4_unsigned",
        r#"
        (module
          ;; i32x4.ne: 5 != 6 -> -1; 5 != 5 -> 0
          (func (result i32)
            (i32x4.extract_lane 0
              (i32x4.ne (v128.const i32x4 5 5 0 0) (v128.const i32x4 6 5 0 0))))
          (func (result i32)
            (i32x4.extract_lane 1
              (i32x4.ne (v128.const i32x4 5 5 0 0) (v128.const i32x4 6 5 0 0))))
          ;; i32x4.lt_u: (unsigned) 1 < 0xFFFFFFFF -> -1
          (func (result i32)
            (i32x4.extract_lane 0
              (i32x4.lt_u (v128.const i32x4 1  0 0 0)
                          (v128.const i32x4 -1  0 0 0))))
          ;; i32x4.gt_u: (unsigned) 0xFFFFFFFF > 1 -> -1
          (func (result i32)
            (i32x4.extract_lane 0
              (i32x4.gt_u (v128.const i32x4 -1 0 0 0)
                          (v128.const i32x4 1  0 0 0))))
          ;; i32x4.le_u: (unsigned) 1 <= 1 -> -1
          (func (result i32)
            (i32x4.extract_lane 0
              (i32x4.le_u (v128.const i32x4 1 0 0 0)
                          (v128.const i32x4 1 0 0 0))))
          ;; i32x4.ge_u: (unsigned) 0xFFFFFFFF >= 2 -> -1
          (func (result i32)
            (i32x4.extract_lane 0
              (i32x4.ge_u (v128.const i32x4 -1 0 0 0)
                          (v128.const i32x4 2  0 0 0)))))
        "#,
        "assert_eq!(func0(), -1);\n    \
         assert_eq!(func1(), 0);\n    \
         assert_eq!(func2(), -1);\n    \
         assert_eq!(func3(), -1);\n    \
         assert_eq!(func4(), -1);\n    \
         assert_eq!(func5(), -1);",
    );
}

// ---------------------------------------------------------------------------
// Comparisons: i64x2.ne, i64x2.gt_s, i64x2.ge_s
// ---------------------------------------------------------------------------

#[test]
fn compare_i64x2() {
    expect_ok(
        "compare_i64x2",
        r#"
        (module
          ;; i64x2.ne: 1 != 2 -> -1 (all ones as i64)
          (func (result i64)
            (i64x2.extract_lane 0
              (i64x2.ne (v128.const i64x2 1 0) (v128.const i64x2 2 0))))
          ;; i64x2.ne: 1 != 1 -> 0
          (func (result i64)
            (i64x2.extract_lane 0
              (i64x2.ne (v128.const i64x2 1 0) (v128.const i64x2 1 0))))
          ;; i64x2.gt_s: 5 > 3 -> -1
          (func (result i64)
            (i64x2.extract_lane 0
              (i64x2.gt_s (v128.const i64x2 5 0) (v128.const i64x2 3 0))))
          ;; i64x2.ge_s: 3 >= 3 -> -1
          (func (result i64)
            (i64x2.extract_lane 0
              (i64x2.ge_s (v128.const i64x2 3 0) (v128.const i64x2 3 0)))))
        "#,
        "assert_eq!(func0(), -1i64);\n    \
         assert_eq!(func1(), 0i64);\n    \
         assert_eq!(func2(), -1i64);\n    \
         assert_eq!(func3(), -1i64);",
    );
}

// ---------------------------------------------------------------------------
// Comparisons: f32x4.gt, f32x4.ge, f64x2.ne, f64x2.gt
// ---------------------------------------------------------------------------

#[test]
fn compare_float_extra() {
    // f32 mask is read back with i32x4.extract_lane; f64 mask with i64x2.extract_lane.
    expect_ok(
        "compare_float_extra",
        r#"
        (module
          ;; f32x4.gt: 3.0 > 2.0 -> all-ones i32 mask (-1)
          (func (result i32)
            (i32x4.extract_lane 0
              (f32x4.gt (v128.const f32x4 3.0 0 0 0) (v128.const f32x4 2.0 0 0 0))))
          ;; f32x4.gt: 2.0 > 3.0 -> 0
          (func (result i32)
            (i32x4.extract_lane 0
              (f32x4.gt (v128.const f32x4 2.0 0 0 0) (v128.const f32x4 3.0 0 0 0))))
          ;; f32x4.ge: 5.0 >= 5.0 -> -1
          (func (result i32)
            (i32x4.extract_lane 0
              (f32x4.ge (v128.const f32x4 5.0 0 0 0) (v128.const f32x4 5.0 0 0 0))))
          ;; f64x2.ne: 1.0 != 2.0 -> -1 (all-ones i64)
          (func (result i64)
            (i64x2.extract_lane 0
              (f64x2.ne (v128.const f64x2 1.0 0) (v128.const f64x2 2.0 0))))
          ;; f64x2.ne: 1.0 != 1.0 -> 0
          (func (result i64)
            (i64x2.extract_lane 0
              (f64x2.ne (v128.const f64x2 1.0 0) (v128.const f64x2 1.0 0))))
          ;; f64x2.gt: 4.0 > 3.0 -> -1
          (func (result i64)
            (i64x2.extract_lane 0
              (f64x2.gt (v128.const f64x2 4.0 0) (v128.const f64x2 3.0 0)))))
        "#,
        "assert_eq!(func0(), -1);\n    \
         assert_eq!(func1(), 0);\n    \
         assert_eq!(func2(), -1);\n    \
         assert_eq!(func3(), -1i64);\n    \
         assert_eq!(func4(), 0i64);\n    \
         assert_eq!(func5(), -1i64);",
    );
}

// ---------------------------------------------------------------------------
// Shifts: i8x16.shl, i16x8.shr_s, i16x8.shr_u
// ---------------------------------------------------------------------------

#[test]
fn shifts_extra() {
    // shr_s is arithmetic (sign-fills); shr_u is logical (zero-fills).
    // A negative lane distinguishes the two.
    expect_ok(
        "shifts_extra",
        r#"
        (module
          ;; i8x16.shl: 1 << 3 = 8
          (func (result i32)
            (i8x16.extract_lane_s 0
              (i8x16.shl (i8x16.splat (i32.const 1)) (i32.const 3))))
          ;; i16x8.shr_s: -32768 >> 1 = -16384 (arithmetic, sign-filling)
          (func (result i32)
            (i16x8.extract_lane_s 0
              (i16x8.shr_s (v128.const i16x8 -32768 0 0 0 0 0 0 0) (i32.const 1))))
          ;; i16x8.shr_u: 0x8000 >> 1 = 0x4000 (logical, zero-filling)
          (func (result i32)
            (i16x8.extract_lane_u 0
              (i16x8.shr_u (v128.const i16x8 -32768 0 0 0 0 0 0 0) (i32.const 1)))))
        "#,
        "assert_eq!(func0(), 8);\n    \
         assert_eq!(func1(), -16384);\n    \
         assert_eq!(func2(), 0x4000);",
    );
}

// ---------------------------------------------------------------------------
// Saturating unsigned: i16x8.add_sat_u, i16x8.sub_sat_u
// ---------------------------------------------------------------------------

#[test]
fn saturating_u16x8() {
    // Unsigned saturation: values clamp to [0, 65535].
    // 0xFFFF + 1 must saturate to 0xFFFF (not wrap to 0).
    // 0 - 1 must saturate to 0 (not wrap to 0xFFFF).
    expect_ok(
        "sat_u16",
        r#"
        (module
          ;; add_sat_u: 65535 + 1 -> 65535
          (func (result i32)
            (i16x8.extract_lane_u 0
              (i16x8.add_sat_u (v128.const i16x8 65535 0 0 0 0 0 0 0)
                               (v128.const i16x8 1     0 0 0 0 0 0 0))))
          ;; add_sat_u: 60000 + 10000 = 70000 -> 65535
          (func (result i32)
            (i16x8.extract_lane_u 0
              (i16x8.add_sat_u (v128.const i16x8 60000 0 0 0 0 0 0 0)
                               (v128.const i16x8 10000 0 0 0 0 0 0 0))))
          ;; sub_sat_u: 0 - 1 -> 0
          (func (result i32)
            (i16x8.extract_lane_u 0
              (i16x8.sub_sat_u (v128.const i16x8 0 0 0 0 0 0 0 0)
                               (v128.const i16x8 1 0 0 0 0 0 0 0))))
          ;; sub_sat_u: 10 - 3 = 7 (no saturation)
          (func (result i32)
            (i16x8.extract_lane_u 0
              (i16x8.sub_sat_u (v128.const i16x8 10 0 0 0 0 0 0 0)
                               (v128.const i16x8 3  0 0 0 0 0 0 0)))))
        "#,
        "assert_eq!(func0(), 65535);\n    \
         assert_eq!(func1(), 65535);\n    \
         assert_eq!(func2(), 0);\n    \
         assert_eq!(func3(), 7);",
    );
}

// ---------------------------------------------------------------------------
// Extend high: i16x8.extend_high_i8x16_u, i32x4.extend_high_i16x8_s,
//              i32x4.extend_high_i16x8_u, i64x2.extend_high_i32x4_s
// ---------------------------------------------------------------------------

#[test]
fn extend_high() {
    // The HIGH variants read source bytes 8..16 (lanes 8..16 for i8, 4..8 for i16,
    // 2..4 for i32). We put distinct values in the upper half so we can prove we
    // are reading the HIGH half and not the low half.
    expect_ok(
        "extend_high",
        r#"
        (module
          ;; i16x8.extend_high_i8x16_u: reads source bytes 8..16 as u8, widens to u16.
          ;; Lane 0 of result = byte 8 of source = 200 (unsigned). Low half has 0.
          (func (result i32)
            (i16x8.extract_lane_u 0
              (i16x8.extend_high_i8x16_u
                (v128.const i8x16 0 0 0 0 0 0 0 0 200 0 0 0 0 0 0 0))))
          ;; i32x4.extend_high_i16x8_s: reads source lanes 4..8 (bytes 8..16) as i16.
          ;; Source: low half = 0, high half lane0 (bytes 8..9) = -100.
          (func (result i32)
            (i32x4.extract_lane 0
              (i32x4.extend_high_i16x8_s
                (v128.const i16x8 0 0 0 0 -100 0 0 0))))
          ;; i32x4.extend_high_i16x8_u: upper-half lane = 0x8000 -> 32768 unsigned.
          (func (result i32)
            (i32x4.extract_lane 0
              (i32x4.extend_high_i16x8_u
                (v128.const i16x8 0 0 0 0 -32768 0 0 0))))
          ;; i64x2.extend_high_i32x4_s: reads source lanes 2..4 (bytes 8..16) as i32.
          ;; Upper lane0 (bytes 8..12) = -77.
          (func (result i64)
            (i64x2.extract_lane 0
              (i64x2.extend_high_i32x4_s
                (v128.const i32x4 1 2 -77 4)))))
        "#,
        // high_u i8->i16: byte8=200 unsigned = 200
        // high_s i16->i32: lane4=-100 signed = -100
        // high_u i16->i32: lane4=0x8000 unsigned = 32768
        // high_s i32->i64: lane2=-77 signed = -77
        "assert_eq!(func0(), 200);\n    \
         assert_eq!(func1(), -100);\n    \
         assert_eq!(func2(), 32768);\n    \
         assert_eq!(func3(), -77i64);",
    );
}

// ---------------------------------------------------------------------------
// Narrow unsigned: i16x8.narrow_i32x4_u
// ---------------------------------------------------------------------------

#[test]
fn narrow_i32x4_u() {
    // i16x8.narrow_i32x4_u: clamps i32 lanes to [0, 65535].
    // Negative inputs -> 0; inputs > 65535 -> 65535.
    // The result packs first vector's lanes into low 8 bytes, second into high 8.
    expect_ok(
        "narrow_i32x4_u",
        r#"
        (module
          ;; 70000 -> 65535 (saturate high)
          (func (result i32)
            (i16x8.extract_lane_u 0
              (i16x8.narrow_i32x4_u (v128.const i32x4 70000 -5 100 0)
                                    (v128.const i32x4 0 0 0 0))))
          ;; -5 -> 0 (saturate low)
          (func (result i32)
            (i16x8.extract_lane_u 1
              (i16x8.narrow_i32x4_u (v128.const i32x4 70000 -5 100 0)
                                    (v128.const i32x4 0 0 0 0))))
          ;; 100 -> 100 (no saturation)
          (func (result i32)
            (i16x8.extract_lane_u 2
              (i16x8.narrow_i32x4_u (v128.const i32x4 70000 -5 100 0)
                                    (v128.const i32x4 0 0 0 0))))
          ;; Second vector's lane0 in result lane4: 200 -> 200
          (func (result i32)
            (i16x8.extract_lane_u 4
              (i16x8.narrow_i32x4_u (v128.const i32x4 0 0 0 0)
                                    (v128.const i32x4 200 0 0 0)))))
        "#,
        "assert_eq!(func0(), 65535);\n    \
         assert_eq!(func1(), 0);\n    \
         assert_eq!(func2(), 100);\n    \
         assert_eq!(func3(), 200);",
    );
}

// ---------------------------------------------------------------------------
// ExtMul high: i16x8.extmul_high_i8x16_u, i32x4.extmul_high_i16x8_s,
//              i32x4.extmul_high_i16x8_u, i64x2.extmul_high_i32x4_s,
//              i64x2.extmul_high_i32x4_u
// ---------------------------------------------------------------------------

#[test]
fn extmul_high() {
    // ExtMul HIGH reads the upper half of both vectors (bytes 8..16), widens each
    // lane, and multiplies pairwise. Low-half values are zeroed to confirm we
    // actually use the high half.
    expect_ok(
        "extmul_high",
        r#"
        (module
          ;; i16x8.extmul_high_i8x16_u: upper half byte0 (byte8) of each = 50 -> 50*50=2500
          (func (result i32)
            (i16x8.extract_lane_u 0
              (i16x8.extmul_high_i8x16_u
                (v128.const i8x16 0 0 0 0 0 0 0 0 50 0 0 0 0 0 0 0)
                (v128.const i8x16 0 0 0 0 0 0 0 0 50 0 0 0 0 0 0 0))))
          ;; i32x4.extmul_high_i16x8_s: upper half lane0 (lane4, bytes 8..10) = -3 -> (-3)*(-3)=9
          (func (result i32)
            (i32x4.extract_lane 0
              (i32x4.extmul_high_i16x8_s
                (v128.const i16x8 0 0 0 0 -3 0 0 0)
                (v128.const i16x8 0 0 0 0 -3 0 0 0))))
          ;; i32x4.extmul_high_i16x8_u: upper half lane0 = 0x8000 (32768) -> 32768^2 = 1073741824
          (func (result i32)
            (i32x4.extract_lane 0
              (i32x4.extmul_high_i16x8_u
                (v128.const i16x8 0 0 0 0 -32768 0 0 0)
                (v128.const i16x8 0 0 0 0 1      0 0 0))))
          ;; i64x2.extmul_high_i32x4_s: upper half lane0 = lane2 = -10 -> (-10)*4=-40
          (func (result i64)
            (i64x2.extract_lane 0
              (i64x2.extmul_high_i32x4_s
                (v128.const i32x4 0 0 -10 0)
                (v128.const i32x4 0 0 4   0))))
          ;; i64x2.extmul_high_i32x4_u: upper half lane0 = lane2 = 0xFFFFFFFF unsigned = 4294967295
          ;; 4294967295 * 1 = 4294967295
          (func (result i64)
            (i64x2.extract_lane 0
              (i64x2.extmul_high_i32x4_u
                (v128.const i32x4 0 0 -1 0)
                (v128.const i32x4 0 0 1  0)))))
        "#,
        "assert_eq!(func0(), 2500);\n    \
         assert_eq!(func1(), 9);\n    \
         assert_eq!(func2(), 32768);\n    \
         assert_eq!(func3(), -40i64);\n    \
         assert_eq!(func4(), 4294967295i64);",
    );
}

// ---------------------------------------------------------------------------
// Pairwise: i16x8.extadd_pairwise_i8x16_u, i32x4.extadd_pairwise_i16x8_s
// ---------------------------------------------------------------------------

#[test]
fn extadd_pairwise_extra() {
    // extadd_pairwise widens adjacent lane pairs and adds them.
    // i8x16_u: unsigned u8 pairs -> i16 sums.
    // i16x8_s: signed i16 pairs -> i32 sums.
    expect_ok(
        "extadd_pairwise_extra",
        r#"
        (module
          ;; i16x8.extadd_pairwise_i8x16_u: lane0 = byte0 + byte1 as u8 = 200 + 100 = 300
          (func (result i32)
            (i16x8.extract_lane_u 0
              (i16x8.extadd_pairwise_i8x16_u
                (v128.const i8x16 200 100 0 0 0 0 0 0 0 0 0 0 0 0 0 0))))
          ;; lane1 = byte2 + byte3 = 50 + 50 = 100
          (func (result i32)
            (i16x8.extract_lane_u 1
              (i16x8.extadd_pairwise_i8x16_u
                (v128.const i8x16 0 0 50 50 0 0 0 0 0 0 0 0 0 0 0 0))))
          ;; i32x4.extadd_pairwise_i16x8_s: lane0 = lane0 + lane1 signed = -1000 + 500 = -500
          (func (result i32)
            (i32x4.extract_lane 0
              (i32x4.extadd_pairwise_i16x8_s
                (v128.const i16x8 -1000 500 0 0 0 0 0 0))))
          ;; lane1 = lane2 + lane3 = 30000 + 30000 = 60000
          (func (result i32)
            (i32x4.extract_lane 1
              (i32x4.extadd_pairwise_i16x8_s
                (v128.const i16x8 0 0 30000 30000 0 0 0 0)))))
        "#,
        "assert_eq!(func0(), 300);\n    \
         assert_eq!(func1(), 100);\n    \
         assert_eq!(func2(), -500);\n    \
         assert_eq!(func3(), 60000);",
    );
}

// ---------------------------------------------------------------------------
// Reduce: i16x8.all_true
// ---------------------------------------------------------------------------

#[test]
fn all_true_i16x8() {
    // all_true returns 1 if every lane is non-zero, else 0.
    expect_ok(
        "all_true_i16x8",
        r#"
        (module
          ;; all lanes non-zero -> 1
          (func (result i32)
            (i16x8.all_true (v128.const i16x8 1 2 3 4 5 6 7 8)))
          ;; one lane is zero -> 0
          (func (result i32)
            (i16x8.all_true (v128.const i16x8 1 2 0 4 5 6 7 8))))
        "#,
        "assert_eq!(func0(), 1);\n    \
         assert_eq!(func1(), 0);",
    );
}

// ---------------------------------------------------------------------------
// Lane load: v128.load16_lane
// ---------------------------------------------------------------------------

#[test]
fn v128_load16_lane() {
    // v128.load16_lane loads a 16-bit value from memory into the named lane of a
    // v128. Other lanes remain unchanged. We store 0x1234 at byte offset 8 and
    // load it into lane 2 of a zero vector, then read it back.
    expect_ok(
        "load16_lane",
        r#"
        (module
          (memory 1)
          (func (result i32)
            ;; Store 0x1234 at memory offset 8
            (i32.store16 (i32.const 8) (i32.const 0x1234))
            ;; Load 16 bits from offset 8 into lane 2 of a zero vector.
          ;; WAT S-expr: addr pushed first (low on stack), then vec on top.
          ;; The transpiler pops value (top) then addr, so addr comes first in S-expr.
            (i16x8.extract_lane_u 2
              (v128.load16_lane 2 (i32.const 8) (v128.const i16x8 0 0 0 0 0 0 0 0)))))
        "#,
        "let mut inst = Instance::new();\n    assert_eq!(inst.func0(), 0x1234);",
    );
}
