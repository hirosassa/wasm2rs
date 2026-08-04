//! Integration tests for the SIMD/v128 proposal (round 1: the foundation
//! vertical slice). A v128 value is represented as a Rust `u128`, so generated
//! functions returning v128 return `u128`. Lane operations are validated
//! end-to-end by building a vector (const/splat/replace_lane), operating on it,
//! and reading a lane back (extract_lane) as an i32/i64/f32/f64 the test asserts
//! on. All modules are stateless unless they declare a memory.

use std::process::Command;

fn compile(test: &str, wat: &str, main_body: &str) -> std::path::PathBuf {
    let wasm = wat::parse_str(wat).expect("valid wat");
    let generated = wasm2rs::transpile(&wasm).expect("transpile ok");

    let dir = std::env::temp_dir().join(format!("wasm2rs_simd_{test}"));
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

#[test]
fn v128_const_and_i32x4_extract_lane() {
    // A v128.const literal, with a chosen i32 lane read back by extract_lane.
    expect_ok(
        "const_extract_i32x4",
        r#"
        (module
          (func (param i32) (result i32)
            (i32x4.extract_lane 0 (v128.const i32x4 10 20 30 40)))
          (func (param i32) (result i32)
            (i32x4.extract_lane 3 (v128.const i32x4 10 20 30 40))))
        "#,
        "assert_eq!(func0(0), 10);\n    assert_eq!(func1(0), 40);",
    );
}

#[test]
fn splat_and_extract_integer_lanes_sign_and_zero_extend() {
    // splat broadcasts a scalar to every lane; extract_lane_s sign-extends and
    // extract_lane_u zero-extends the sub-word lanes back into an i32.
    expect_ok(
        "splat_extract_int",
        r#"
        (module
          (func (param i32) (result i32)
            (i8x16.extract_lane_s 5 (i8x16.splat (local.get 0))))
          (func (param i32) (result i32)
            (i8x16.extract_lane_u 5 (i8x16.splat (local.get 0))))
          (func (param i32) (result i32)
            (i16x8.extract_lane_s 3 (i16x8.splat (local.get 0))))
          (func (param i32) (result i32)
            (i16x8.extract_lane_u 3 (i16x8.splat (local.get 0))))
          (func (param i32) (result i32)
            (i32x4.extract_lane 2 (i32x4.splat (local.get 0))))
          (func (param i64) (result i64)
            (i64x2.extract_lane 1 (i64x2.splat (local.get 0)))))
        "#,
        "assert_eq!(func0(0xFF), -1);\n    \
         assert_eq!(func1(0xFF), 255);\n    \
         assert_eq!(func2(0x8000), -32768);\n    \
         assert_eq!(func3(0x8000), 32768);\n    \
         assert_eq!(func4(-123456), -123456);\n    \
         assert_eq!(func5(0x1_0000_0000i64), 0x1_0000_0000i64);",
    );
}

#[test]
fn splat_and_extract_float_lanes() {
    expect_ok(
        "splat_extract_float",
        r#"
        (module
          (func (param f32) (result f32)
            (f32x4.extract_lane 2 (f32x4.splat (local.get 0))))
          (func (param f64) (result f64)
            (f64x2.extract_lane 1 (f64x2.splat (local.get 0)))))
        "#,
        "assert_eq!(func0(3.5f32), 3.5f32);\n    \
         assert_eq!(func1(-2.25f64), -2.25f64);",
    );
}

#[test]
fn replace_lane_changes_only_the_named_lane() {
    // Replacing lane 1 must leave the other lanes intact.
    expect_ok(
        "replace_lane",
        r#"
        (module
          (func (param i32) (result i32)
            (i32x4.extract_lane 1
              (i32x4.replace_lane 1 (v128.const i32x4 10 20 30 40) (local.get 0))))
          (func (param i32) (result i32)
            (i32x4.extract_lane 2
              (i32x4.replace_lane 1 (v128.const i32x4 10 20 30 40) (local.get 0))))
          (func (param f64) (result f64)
            (f64x2.extract_lane 0
              (f64x2.replace_lane 0 (v128.const f64x2 1.5 2.5) (local.get 0)))))
        "#,
        "assert_eq!(func0(99), 99);\n    \
         assert_eq!(func1(99), 30);\n    \
         assert_eq!(func2(7.75f64), 7.75f64);",
    );
}

#[test]
fn v128_load_store_roundtrip() {
    // Store a v128 to linear memory and load it back, reading a lane to confirm
    // the 16 bytes survive the round trip.
    expect_ok(
        "load_store",
        r#"
        (module
          (memory 1)
          (func (param i32) (result i32)
            (v128.store (i32.const 16) (v128.const i32x4 111 222 333 444))
            (i32x4.extract_lane 2 (v128.load (i32.const 16)))))
        "#,
        // A module with a memory becomes a `struct Instance`; the function is a
        // method on it.
        "let mut inst = Instance::new();\n    assert_eq!(inst.func0(0), 333);",
    );
}

#[test]
fn v128_bitwise_ops() {
    expect_ok(
        "bitwise",
        r#"
        (module
          (func (result i32)
            (i32x4.extract_lane 0
              (v128.and (v128.const i32x4 0xF0F0F0F0 0 0 0)
                        (v128.const i32x4 0xFF00FF00 0 0 0))))
          (func (result i32)
            (i32x4.extract_lane 0
              (v128.or (v128.const i32x4 0xF0F0F0F0 0 0 0)
                       (v128.const i32x4 0x0F0F0F0F 0 0 0))))
          (func (result i32)
            (i32x4.extract_lane 0
              (v128.xor (v128.const i32x4 0xFF00FF00 0 0 0)
                        (v128.const i32x4 0x0F0F0F0F 0 0 0))))
          (func (result i32)
            (i32x4.extract_lane 0
              (v128.not (v128.const i32x4 0x00000000 0 0 0))))
          (func (result i32)
            (i32x4.extract_lane 0
              (v128.andnot (v128.const i32x4 0xFF00FF00 0 0 0)
                           (v128.const i32x4 0x0F00F000 0 0 0)))))
        "#,
        "assert_eq!(func0() as u32, 0xF000F000);\n    \
         assert_eq!(func1() as u32, 0xFFFFFFFF);\n    \
         assert_eq!(func2() as u32, 0xF00FF00F);\n    \
         assert_eq!(func3() as u32, 0xFFFFFFFF);\n    \
         assert_eq!(func4() as u32, 0xF000_0F00);",
    );
}

#[test]
fn integer_lane_add_sub_mul() {
    // Lane-wise wrapping arithmetic. Build vectors with splat/replace, operate,
    // read a lane back. i8x16 lanes wrap at 8 bits.
    expect_ok(
        "int_lane_arith",
        r#"
        (module
          (func (result i32)
            (i8x16.extract_lane_s 0
              (i8x16.add (i8x16.splat (i32.const 100)) (i8x16.splat (i32.const 100)))))
          (func (result i32)
            (i16x8.extract_lane_s 0
              (i16x8.sub (v128.const i16x8 5 0 0 0 0 0 0 0)
                         (v128.const i16x8 8 0 0 0 0 0 0 0))))
          (func (result i32)
            (i32x4.extract_lane 2
              (i32x4.mul (v128.const i32x4 0 0 7 0) (v128.const i32x4 0 0 6 0))))
          (func (result i64)
            (i64x2.extract_lane 1
              (i64x2.add (v128.const i64x2 0 1000000000000)
                         (v128.const i64x2 0 337203685477)))))
        "#,
        // 100+100 = 200 as i8 wraps to -56; 5-8 = -3; 7*6 = 42;
        // 1_000_000_000_000 + 337_203_685_477 = 1_337_203_685_477 (i64 lane).
        "assert_eq!(func0(), -56);\n    \
         assert_eq!(func1(), -3);\n    \
         assert_eq!(func2(), 42);\n    \
         assert_eq!(func3(), 1337203685477i64);",
    );
}

#[test]
fn integer_lane_neg() {
    expect_ok(
        "int_lane_neg",
        r#"
        (module
          (func (result i32)
            (i32x4.extract_lane 1 (i32x4.neg (v128.const i32x4 10 20 30 40))))
          (func (result i32)
            (i8x16.extract_lane_s 0 (i8x16.neg (i8x16.splat (i32.const 1))))))
        "#,
        "assert_eq!(func0(), -20);\n    assert_eq!(func1(), -1);",
    );
}

#[test]
fn float_lane_binary_arith() {
    // Lane-wise f32x4/f64x2 add/sub/mul/div. All chosen values are exact in
    // binary floating point so equality holds.
    expect_ok(
        "float_lane_binary",
        r#"
        (module
          (func (result f32)
            (f32x4.extract_lane 0
              (f32x4.add (v128.const f32x4 1.5 2.5 3.5 4.5)
                         (v128.const f32x4 0.5 0.5 0.5 0.5))))
          (func (result f32)
            (f32x4.extract_lane 1
              (f32x4.sub (v128.const f32x4 10 20 30 40)
                         (v128.const f32x4 1 2 3 4))))
          (func (result f32)
            (f32x4.extract_lane 2
              (f32x4.mul (v128.const f32x4 1 2 3 4)
                         (v128.const f32x4 5 5 5 5))))
          (func (result f64)
            (f64x2.extract_lane 0
              (f64x2.div (v128.const f64x2 9 8) (v128.const f64x2 3 2))))
          (func (result f64)
            (f64x2.extract_lane 1
              (f64x2.add (v128.const f64x2 1.25 2.5) (v128.const f64x2 0.25 0.5)))))
        "#,
        "assert_eq!(func0(), 2.0f32);\n    \
         assert_eq!(func1(), 18.0f32);\n    \
         assert_eq!(func2(), 15.0f32);\n    \
         assert_eq!(func3(), 3.0f64);\n    \
         assert_eq!(func4(), 3.0f64);",
    );
}

#[test]
fn float_lane_neg_abs_sqrt() {
    // neg/abs are sign-bit rewrites (bit-exact, even for NaN); sqrt is per-lane.
    expect_ok(
        "float_lane_unary",
        r#"
        (module
          (func (result f32)
            (f32x4.extract_lane 0 (f32x4.neg (v128.const f32x4 2.5 0 0 0))))
          (func (result f32)
            (f32x4.extract_lane 0 (f32x4.abs (v128.const f32x4 -3.5 0 0 0))))
          (func (result f32)
            (f32x4.extract_lane 0 (f32x4.sqrt (v128.const f32x4 16 0 0 0))))
          (func (result f64)
            (f64x2.extract_lane 1 (f64x2.neg (v128.const f64x2 1 -4))))
          (func (result f64)
            (f64x2.extract_lane 1 (f64x2.abs (v128.const f64x2 1 -4))))
          (func (result f64)
            (f64x2.extract_lane 0 (f64x2.sqrt (v128.const f64x2 9 0))))
          ;; A high f32 lane (lane 3), to confirm the neg sign mask is tiled
          ;; across the whole register, not just the low lane.
          (func (result f32)
            (f32x4.extract_lane 3 (f32x4.neg (v128.const f32x4 0 0 0 6.5)))))
        "#,
        "assert_eq!(func0(), -2.5f32);\n    \
         assert_eq!(func1(), 3.5f32);\n    \
         assert_eq!(func2(), 4.0f32);\n    \
         assert_eq!(func3(), 4.0f64);\n    \
         assert_eq!(func4(), 4.0f64);\n    \
         assert_eq!(func5(), 3.0f64);\n    \
         assert_eq!(func6(), -6.5f32);",
    );
}

#[test]
fn float_lane_min_max() {
    // wasm min/max return NaN if either operand is NaN, and for equal operands
    // (±0) min keeps the negative, max the positive — mirroring the scalar
    // `f32_min`/`f32_max` helpers.
    expect_ok(
        "float_lane_minmax",
        r#"
        (module
          (func (result f32)
            (f32x4.extract_lane 0
              (f32x4.min (v128.const f32x4 3 3 3 3) (v128.const f32x4 5 5 5 5))))
          (func (result f32)
            (f32x4.extract_lane 0
              (f32x4.max (v128.const f32x4 3 3 3 3) (v128.const f32x4 5 5 5 5))))
          (func (result f32)
            (f32x4.extract_lane 0
              (f32x4.min (v128.const f32x4 nan 0 0 0) (v128.const f32x4 1 1 1 1))))
          (func (result f32)
            (f32x4.extract_lane 0
              (f32x4.min (v128.const f32x4 -0.0 0 0 0) (v128.const f32x4 0.0 0 0 0))))
          (func (result f32)
            (f32x4.extract_lane 0
              (f32x4.max (v128.const f32x4 -0.0 0 0 0) (v128.const f32x4 0.0 0 0 0)))))
        "#,
        "assert_eq!(func0(), 3.0f32);\n    \
         assert_eq!(func1(), 5.0f32);\n    \
         assert!(func2().is_nan());\n    \
         assert!(func3().is_sign_negative() && func3() == 0.0f32);\n    \
         assert!(!func4().is_sign_negative() && func4() == 0.0f32);",
    );
}

#[test]
fn float_lane_pmin_pmax() {
    // Pseudo-min/max: pmin(a,b)=b<a?b:a, pmax(a,b)=a<b?b:a. With a NaN operand
    // the comparison is false, so both return the first operand.
    expect_ok(
        "float_lane_pminmax",
        r#"
        (module
          (func (result f32)
            (f32x4.extract_lane 0
              (f32x4.pmin (v128.const f32x4 3 3 3 3) (v128.const f32x4 5 5 5 5))))
          (func (result f32)
            (f32x4.extract_lane 0
              (f32x4.pmax (v128.const f32x4 3 3 3 3) (v128.const f32x4 5 5 5 5))))
          (func (result f64)
            (f64x2.extract_lane 0
              (f64x2.pmin (v128.const f64x2 1 1) (v128.const f64x2 nan nan))))
          (func (result f64)
            (f64x2.extract_lane 0
              (f64x2.pmax (v128.const f64x2 1 1) (v128.const f64x2 nan nan)))))
        "#,
        "assert_eq!(func0(), 3.0f32);\n    \
         assert_eq!(func1(), 5.0f32);\n    \
         assert_eq!(func2(), 1.0f64);\n    \
         assert_eq!(func3(), 1.0f64);",
    );
}

#[test]
fn float_lane_rounding() {
    // ceil/floor/trunc and nearest (round half to even).
    expect_ok(
        "float_lane_rounding",
        r#"
        (module
          (func (result f32)
            (f32x4.extract_lane 0 (f32x4.ceil (v128.const f32x4 2.3 0 0 0))))
          (func (result f32)
            (f32x4.extract_lane 0 (f32x4.floor (v128.const f32x4 2.7 0 0 0))))
          (func (result f32)
            (f32x4.extract_lane 0 (f32x4.trunc (v128.const f32x4 -2.9 0 0 0))))
          (func (result f64)
            (f64x2.extract_lane 0 (f64x2.nearest (v128.const f64x2 2.5 3.5))))
          (func (result f64)
            (f64x2.extract_lane 1 (f64x2.nearest (v128.const f64x2 2.5 3.5)))))
        "#,
        "assert_eq!(func0(), 3.0f32);\n    \
         assert_eq!(func1(), 2.0f32);\n    \
         assert_eq!(func2(), -2.0f32);\n    \
         assert_eq!(func3(), 2.0f64);\n    \
         assert_eq!(func4(), 4.0f64);",
    );
}

#[test]
fn lane_compare_integer() {
    // Lane comparisons yield an all-ones mask (read back as -1 via a signed
    // extract) where the predicate holds, else zero. lt_s and lt_u differ on the
    // sign bit: 0x80 is -128 signed but 128 unsigned.
    expect_ok(
        "lane_compare_int",
        r#"
        (module
          (func (result i32)
            (i32x4.extract_lane 0
              (i32x4.eq (v128.const i32x4 1 2 3 4) (v128.const i32x4 1 0 3 0))))
          (func (result i32)
            (i32x4.extract_lane 1
              (i32x4.eq (v128.const i32x4 1 2 3 4) (v128.const i32x4 1 0 3 0))))
          (func (result i32)
            (i8x16.extract_lane_s 0
              (i8x16.lt_s (i8x16.splat (i32.const 0x80)) (i8x16.splat (i32.const 1)))))
          (func (result i32)
            (i8x16.extract_lane_s 0
              (i8x16.lt_u (i8x16.splat (i32.const 0x80)) (i8x16.splat (i32.const 1)))))
          (func (result i32)
            (i16x8.extract_lane_s 0
              (i16x8.ge_s (v128.const i16x8 5 0 0 0 0 0 0 0)
                          (v128.const i16x8 5 0 0 0 0 0 0 0)))))
        "#,
        "assert_eq!(func0(), -1);\n    \
         assert_eq!(func1(), 0);\n    \
         assert_eq!(func2(), -1);\n    \
         assert_eq!(func3(), 0);\n    \
         assert_eq!(func4(), -1);",
    );
}

#[test]
fn lane_compare_float() {
    // Float lane comparisons; NaN makes eq false and ne true. The 32-/64-bit
    // masks are read with i32x4/i64x2 extract_lane.
    expect_ok(
        "lane_compare_float",
        r#"
        (module
          (func (result i32)
            (i32x4.extract_lane 0
              (f32x4.lt (v128.const f32x4 1 2 3 4) (v128.const f32x4 2 2 2 2))))
          (func (result i32)
            (i32x4.extract_lane 1
              (f32x4.lt (v128.const f32x4 1 2 3 4) (v128.const f32x4 2 2 2 2))))
          (func (result i32)
            (i32x4.extract_lane 0
              (f32x4.eq (v128.const f32x4 nan 0 0 0) (v128.const f32x4 nan 0 0 0))))
          (func (result i32)
            (i32x4.extract_lane 0
              (f32x4.ne (v128.const f32x4 nan 0 0 0) (v128.const f32x4 nan 0 0 0))))
          (func (result i64)
            (i64x2.extract_lane 0
              (f64x2.ge (v128.const f64x2 5 1) (v128.const f64x2 5 2)))))
        "#,
        "assert_eq!(func0(), -1);\n    \
         assert_eq!(func1(), 0);\n    \
         assert_eq!(func2(), 0);\n    \
         assert_eq!(func3(), -1);\n    \
         assert_eq!(func4(), -1i64);",
    );
}

#[test]
fn lane_shift() {
    // Lane shifts take an i32 count modulo the lane width. shr_s is arithmetic
    // (sign-extending), shr_u logical.
    expect_ok(
        "lane_shift",
        r#"
        (module
          (func (result i32)
            (i32x4.extract_lane 0 (i32x4.shl (v128.const i32x4 1 0 0 0) (i32.const 4))))
          (func (result i32)
            (i8x16.extract_lane_s 0 (i8x16.shr_s (i8x16.splat (i32.const 0x80)) (i32.const 1))))
          (func (result i32)
            (i8x16.extract_lane_u 0 (i8x16.shr_u (i8x16.splat (i32.const 0x80)) (i32.const 1))))
          (func (result i64)
            (i64x2.extract_lane 0 (i64x2.shl (v128.const i64x2 1 0) (i32.const 40))))
          (func (result i32)
            (i32x4.extract_lane 0 (i32x4.shl (v128.const i32x4 1 0 0 0) (i32.const 36)))))
        "#,
        // 1<<4 = 16; -128>>1 = -64; 128>>1 = 64; 1<<40; 36 mod 32 = 4 so 1<<4 = 16.
        "assert_eq!(func0(), 16);\n    \
         assert_eq!(func1(), -64);\n    \
         assert_eq!(func2(), 64);\n    \
         assert_eq!(func3(), 1i64 << 40);\n    \
         assert_eq!(func4(), 16);",
    );
}

#[test]
fn lane_add_sub_sat() {
    // Saturating add/sub clamp to the lane's range instead of wrapping. Unlike
    // the wrapping helpers these are signedness-dependent: `_s` clamps to the
    // signed range, `_u` to the unsigned one.
    expect_ok(
        "lane_add_sub_sat",
        r#"
        (module
          (func (result i32)
            (i8x16.extract_lane_s 0
              (i8x16.add_sat_s (i8x16.splat (i32.const 127)) (i8x16.splat (i32.const 10)))))
          (func (result i32)
            (i8x16.extract_lane_s 0
              (i8x16.add_sat_s (i8x16.splat (i32.const -128)) (i8x16.splat (i32.const -10)))))
          (func (result i32)
            (i8x16.extract_lane_u 0
              (i8x16.add_sat_u (i8x16.splat (i32.const 255)) (i8x16.splat (i32.const 10)))))
          (func (result i32)
            (i8x16.extract_lane_u 0
              (i8x16.sub_sat_u (i8x16.splat (i32.const 0)) (i8x16.splat (i32.const 10)))))
          (func (result i32)
            (i16x8.extract_lane_s 0
              (i16x8.add_sat_s (i16x8.splat (i32.const 32767)) (i16x8.splat (i32.const 1)))))
          (func (result i32)
            (i16x8.extract_lane_s 0
              (i16x8.sub_sat_s (i16x8.splat (i32.const -32768)) (i16x8.splat (i32.const 1))))))
        "#,
        "assert_eq!(func0(), 127);\n    \
         assert_eq!(func1(), -128);\n    \
         assert_eq!(func2(), 255);\n    \
         assert_eq!(func3(), 0);\n    \
         assert_eq!(func4(), 32767);\n    \
         assert_eq!(func5(), -32768);",
    );
}

#[test]
fn lane_extend() {
    // extend_low/high sign- (`_s`) or zero-extend (`_u`) the low or high half of
    // the lanes to double width. The high variants read source bytes 8..16.
    expect_ok(
        "lane_extend",
        r#"
        (module
          (func (result i32)
            (i16x8.extract_lane_s 0
              (i16x8.extend_low_i8x16_s
                (v128.const i8x16 -1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16))))
          (func (result i32)
            (i16x8.extract_lane_u 0
              (i16x8.extend_low_i8x16_u
                (v128.const i8x16 -1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16))))
          (func (result i32)
            (i16x8.extract_lane_s 0
              (i16x8.extend_high_i8x16_s
                (v128.const i8x16 -1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16))))
          (func (result i32)
            (i32x4.extract_lane 0
              (i32x4.extend_low_i16x8_s (v128.const i16x8 -1 2 3 4 5 6 7 8))))
          (func (result i64)
            (i64x2.extract_lane 0
              (i64x2.extend_high_i32x4_u (v128.const i32x4 -1 2 3 4)))))
        "#,
        // low_s lane0 = -1; low_u lane0 = 255; high_s lane0 = byte8 = 9;
        // i32 low_s lane0 = -1; i64 high_u lane0 = source lane2 = 3.
        "assert_eq!(func0(), -1);\n    \
         assert_eq!(func1(), 255);\n    \
         assert_eq!(func2(), 9);\n    \
         assert_eq!(func3(), -1);\n    \
         assert_eq!(func4(), 3i64);",
    );
}

#[test]
fn lane_narrow() {
    // narrow saturates two vectors' lanes to half width (source read signed),
    // concatenating the first vector's lanes then the second's. `_s` saturates
    // to the signed range, `_u` to the unsigned one.
    expect_ok(
        "lane_narrow",
        r#"
        (module
          (func (result i32)
            (i8x16.extract_lane_s 0
              (i8x16.narrow_i16x8_s (v128.const i16x8 300 -300 5 0 0 0 0 0)
                                    (v128.const i16x8 100 0 0 0 0 0 0 0))))
          (func (result i32)
            (i8x16.extract_lane_s 1
              (i8x16.narrow_i16x8_s (v128.const i16x8 300 -300 5 0 0 0 0 0)
                                    (v128.const i16x8 100 0 0 0 0 0 0 0))))
          (func (result i32)
            (i8x16.extract_lane_s 8
              (i8x16.narrow_i16x8_s (v128.const i16x8 300 -300 5 0 0 0 0 0)
                                    (v128.const i16x8 100 0 0 0 0 0 0 0))))
          (func (result i32)
            (i8x16.extract_lane_u 0
              (i8x16.narrow_i16x8_u (v128.const i16x8 300 -5 0 0 0 0 0 0)
                                    (v128.const i16x8 0 0 0 0 0 0 0 0))))
          (func (result i32)
            (i8x16.extract_lane_u 1
              (i8x16.narrow_i16x8_u (v128.const i16x8 300 -5 0 0 0 0 0 0)
                                    (v128.const i16x8 0 0 0 0 0 0 0 0))))
          (func (result i32)
            (i16x8.extract_lane_s 0
              (i16x8.narrow_i32x4_s (v128.const i32x4 100000 -100000 0 0)
                                    (v128.const i32x4 0 0 0 0))))
          (func (result i32)
            (i16x8.extract_lane_s 1
              (i16x8.narrow_i32x4_s (v128.const i32x4 100000 -100000 0 0)
                                    (v128.const i32x4 0 0 0 0)))))
        "#,
        // s: 300->127, -300->-128, b lane0=100; u: 300->255, -5->0;
        // i16 s: 100000->32767, -100000->-32768.
        "assert_eq!(func0(), 127);\n    \
         assert_eq!(func1(), -128);\n    \
         assert_eq!(func2(), 100);\n    \
         assert_eq!(func3(), 255);\n    \
         assert_eq!(func4(), 0);\n    \
         assert_eq!(func5(), 32767);\n    \
         assert_eq!(func6(), -32768);",
    );
}

#[test]
fn lane_extmul() {
    // extmul_low/high widen the low or high half of both vectors' lanes and
    // multiply them pairwise; the product always fits the double-width lane.
    expect_ok(
        "lane_extmul",
        r#"
        (module
          (func (result i32)
            (i16x8.extract_lane_s 0
              (i16x8.extmul_low_i8x16_s
                (v128.const i8x16 100 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16)
                (v128.const i8x16 100 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16))))
          (func (result i32)
            (i16x8.extract_lane_s 0
              (i16x8.extmul_high_i8x16_s
                (v128.const i8x16 0 0 0 0 0 0 0 0 7 0 0 0 0 0 0 0)
                (v128.const i8x16 0 0 0 0 0 0 0 0 3 0 0 0 0 0 0 0))))
          (func (result i32)
            (i16x8.extract_lane_u 0
              (i16x8.extmul_low_i8x16_u (i8x16.splat (i32.const 200)) (i8x16.splat (i32.const 200)))))
          (func (result i32)
            (i32x4.extract_lane 0
              (i32x4.extmul_low_i16x8_s (v128.const i16x8 1000 0 0 0 0 0 0 0)
                                        (v128.const i16x8 1000 0 0 0 0 0 0 0))))
          (func (result i64)
            (i64x2.extract_lane 0
              (i64x2.extmul_low_i32x4_s (v128.const i32x4 100000 0 0 0)
                                        (v128.const i32x4 100000 0 0 0)))))
        "#,
        // 100*100; high lane0=byte8: 7*3; 200*200 as u16; 1000*1000; 100000*100000.
        "assert_eq!(func0(), 10000);\n    \
         assert_eq!(func1(), 21);\n    \
         assert_eq!(func2(), 40000);\n    \
         assert_eq!(func3(), 1000000);\n    \
         assert_eq!(func4(), 10_000_000_000i64);",
    );
}

#[test]
fn lane_q15mulr_sat() {
    // q15mulr_sat_s: fixed-point Q15 multiply, (a*b + 0x4000) >> 15, saturated
    // to the signed 16-bit range. Only -1.0 * -1.0 saturates.
    expect_ok(
        "lane_q15mulr_sat",
        r#"
        (module
          (func (result i32)
            (i16x8.extract_lane_s 0
              (i16x8.q15mulr_sat_s (i16x8.splat (i32.const 0x4000)) (i16x8.splat (i32.const 0x4000)))))
          (func (result i32)
            (i16x8.extract_lane_s 0
              (i16x8.q15mulr_sat_s (i16x8.splat (i32.const -32768)) (i16x8.splat (i32.const -32768))))))
        "#,
        // (0x4000*0x4000 + 0x4000) >> 15 = 8192; (-32768^2 + 0x4000) >> 15 = 32768 -> 32767.
        "assert_eq!(func0(), 8192);\n    \
         assert_eq!(func1(), 32767);",
    );
}

#[test]
fn lane_extadd_pairwise() {
    // extadd_pairwise widens and sums each adjacent lane pair of one vector.
    expect_ok(
        "lane_extadd_pairwise",
        r#"
        (module
          (func (result i32)
            (i16x8.extract_lane_s 0
              (i16x8.extadd_pairwise_i8x16_s
                (v128.const i8x16 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16))))
          (func (result i32)
            (i16x8.extract_lane_s 1
              (i16x8.extadd_pairwise_i8x16_s
                (v128.const i8x16 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16))))
          (func (result i32)
            (i16x8.extract_lane_s 0
              (i16x8.extadd_pairwise_i8x16_s
                (v128.const i8x16 -1 -2 0 0 0 0 0 0 0 0 0 0 0 0 0 0))))
          (func (result i32)
            (i32x4.extract_lane 0
              (i32x4.extadd_pairwise_i16x8_u (v128.const i16x8 100 200 0 0 0 0 0 0)))))
        "#,
        // 1+2; 3+4; -1+-2; 100+200.
        "assert_eq!(func0(), 3);\n    \
         assert_eq!(func1(), 7);\n    \
         assert_eq!(func2(), -3);\n    \
         assert_eq!(func3(), 300);",
    );
}

#[test]
fn lane_dot() {
    // i32x4.dot_i16x8_s: per output lane, sum the two adjacent i16 products with
    // two's-complement (wrapping) addition of the exact i32 products.
    expect_ok(
        "lane_dot",
        r#"
        (module
          (func (result i32)
            (i32x4.extract_lane 0
              (i32x4.dot_i16x8_s (v128.const i16x8 1 2 3 4 5 6 7 8)
                                 (v128.const i16x8 5 6 7 8 1 2 3 4))))
          (func (result i32)
            (i32x4.extract_lane 1
              (i32x4.dot_i16x8_s (v128.const i16x8 1 2 3 4 5 6 7 8)
                                 (v128.const i16x8 5 6 7 8 1 2 3 4))))
          (func (result i32)
            (i32x4.extract_lane 0
              (i32x4.dot_i16x8_s (v128.const i16x8 -32768 -32768 0 0 0 0 0 0)
                                 (v128.const i16x8 -32768 -32768 0 0 0 0 0 0)))))
        "#,
        // 1*5+2*6=17; 3*7+4*8=53; 2^30+2^30=2^31 wraps to i32::MIN.
        "assert_eq!(func0(), 17);\n    \
         assert_eq!(func1(), 53);\n    \
         assert_eq!(func2(), i32::MIN);",
    );
}

#[test]
fn v128_bitselect_and_any_true() {
    expect_ok(
        "bitselect_anytrue",
        r#"
        (module
          (func (result i32)
            (i32x4.extract_lane 0
              (v128.bitselect (v128.const i32x4 0xAAAAAAAA 0 0 0)
                              (v128.const i32x4 0xBBBBBBBB 0 0 0)
                              (v128.const i32x4 0xFF00FF00 0 0 0))))
          (func (result i32)
            (v128.any_true (v128.const i32x4 0 0 0 0)))
          (func (result i32)
            (v128.any_true (v128.const i32x4 0 0 1 0))))
        "#,
        // bitselect picks bits of the first vector where the mask is 1, the
        // second where it is 0: 0xAA&0xFF | 0xBB&0x00 per byte = 0xAABBAABB.
        "assert_eq!(func0() as u32, 0xAABBAABB);\n    \
         assert_eq!(func1(), 0);\n    \
         assert_eq!(func2(), 1);",
    );
}

#[test]
fn lane_trunc_sat() {
    // i32x4.trunc_sat_f32x4_s/u: saturating truncation matches Rust's float-to-int
    // `as` cast: NaN -> 0, out-of-range clamps to the type's bounds.
    expect_ok(
        "lane_trunc_sat",
        r#"
        (module
          (func (result i32)
            (i32x4.extract_lane 0 (i32x4.trunc_sat_f32x4_s (v128.const f32x4 2.5 nan 1e10 -1e10))))
          (func (result i32)
            (i32x4.extract_lane 1 (i32x4.trunc_sat_f32x4_s (v128.const f32x4 2.5 nan 1e10 -1e10))))
          (func (result i32)
            (i32x4.extract_lane 2 (i32x4.trunc_sat_f32x4_s (v128.const f32x4 2.5 nan 1e10 -1e10))))
          (func (result i32)
            (i32x4.extract_lane 3 (i32x4.trunc_sat_f32x4_s (v128.const f32x4 2.5 nan 1e10 -1e10))))
          (func (result i32)
            (i32x4.extract_lane 0 (i32x4.trunc_sat_f32x4_u (v128.const f32x4 -1.0 3.9 1e10 0))))
          (func (result i32)
            (i32x4.extract_lane 1 (i32x4.trunc_sat_f32x4_u (v128.const f32x4 -1.0 3.9 1e10 0))))
          (func (result i32)
            (i32x4.extract_lane 2 (i32x4.trunc_sat_f32x4_u (v128.const f32x4 -1.0 3.9 1e10 0)))))
        "#,
        // s: 2.5->2, NaN->0, 1e10->i32::MAX, -1e10->i32::MIN.
        // u: -1.0->0, 3.9->3, 1e10->u32::MAX.
        "assert_eq!(func0(), 2);\n    \
         assert_eq!(func1(), 0);\n    \
         assert_eq!(func2(), i32::MAX);\n    \
         assert_eq!(func3(), i32::MIN);\n    \
         assert_eq!(func4(), 0);\n    \
         assert_eq!(func5(), 3);\n    \
         assert_eq!(func6() as u32, u32::MAX);",
    );
}

#[test]
fn lane_trunc_sat_zero() {
    // i32x4.trunc_sat_f64x2_s/u_zero: truncate the two f64 lanes into the low two
    // i32 lanes; the upper two lanes are zero-filled.
    expect_ok(
        "lane_trunc_sat_zero",
        r#"
        (module
          (func (result i32)
            (i32x4.extract_lane 0 (i32x4.trunc_sat_f64x2_s_zero (v128.const f64x2 2.5 -3.5))))
          (func (result i32)
            (i32x4.extract_lane 1 (i32x4.trunc_sat_f64x2_s_zero (v128.const f64x2 2.5 -3.5))))
          (func (result i32)
            (i32x4.extract_lane 2 (i32x4.trunc_sat_f64x2_s_zero (v128.const f64x2 2.5 -3.5))))
          (func (result i32)
            (i32x4.extract_lane 0 (i32x4.trunc_sat_f64x2_u_zero (v128.const f64x2 -1.0 5.9))))
          (func (result i32)
            (i32x4.extract_lane 1 (i32x4.trunc_sat_f64x2_u_zero (v128.const f64x2 -1.0 5.9)))))
        "#,
        // 2.5->2, -3.5->-3, lane2 zero-filled; -1.0->0, 5.9->5.
        "assert_eq!(func0(), 2);\n    \
         assert_eq!(func1(), -3);\n    \
         assert_eq!(func2(), 0);\n    \
         assert_eq!(func3(), 0);\n    \
         assert_eq!(func4(), 5);",
    );
}

#[test]
fn lane_convert() {
    // f32x4.convert_i32x4_s/u: widen each i32 lane to f32. The `_u` variant reads
    // the lane as u32, so lane 0xFFFFFFFF becomes 2^32, not -1.
    expect_ok(
        "lane_convert",
        r#"
        (module
          (func (result f32)
            (f32x4.extract_lane 0 (f32x4.convert_i32x4_s (v128.const i32x4 -5 7 0 0))))
          (func (result f32)
            (f32x4.extract_lane 1 (f32x4.convert_i32x4_s (v128.const i32x4 -5 7 0 0))))
          (func (result f32)
            (f32x4.extract_lane 0 (f32x4.convert_i32x4_u (v128.const i32x4 -1 10 0 0)))))
        "#,
        // -5->-5.0, 7->7.0; unsigned 0xFFFFFFFF = 4294967295 rounds to 2^32.
        "assert_eq!(func0(), -5.0f32);\n    \
         assert_eq!(func1(), 7.0f32);\n    \
         assert_eq!(func2(), 4294967296.0f32);",
    );
}

#[test]
fn lane_convert_low() {
    // f64x2.convert_low_i32x4_s/u: convert the low two i32 lanes to f64; the upper
    // two source lanes are ignored.
    expect_ok(
        "lane_convert_low",
        r#"
        (module
          (func (result f64)
            (f64x2.extract_lane 0 (f64x2.convert_low_i32x4_s (v128.const i32x4 -5 7 999 999))))
          (func (result f64)
            (f64x2.extract_lane 1 (f64x2.convert_low_i32x4_s (v128.const i32x4 -5 7 999 999))))
          (func (result f64)
            (f64x2.extract_lane 0 (f64x2.convert_low_i32x4_u (v128.const i32x4 -1 0 0 0)))))
        "#,
        // -5->-5.0, 7->7.0 (lane 2/3 ignored); unsigned 0xFFFFFFFF = 4294967295.0.
        "assert_eq!(func0(), -5.0f64);\n    \
         assert_eq!(func1(), 7.0f64);\n    \
         assert_eq!(func2(), 4294967295.0f64);",
    );
}

#[test]
fn lane_demote() {
    // f32x4.demote_f64x2_zero: narrow the two f64 lanes to the low two f32 lanes;
    // the upper two lanes are zero-filled.
    expect_ok(
        "lane_demote",
        r#"
        (module
          (func (result f32)
            (f32x4.extract_lane 0 (f32x4.demote_f64x2_zero (v128.const f64x2 1.5 -2.5))))
          (func (result f32)
            (f32x4.extract_lane 1 (f32x4.demote_f64x2_zero (v128.const f64x2 1.5 -2.5))))
          (func (result f32)
            (f32x4.extract_lane 2 (f32x4.demote_f64x2_zero (v128.const f64x2 1.5 -2.5)))))
        "#,
        // 1.5->1.5, -2.5->-2.5, lane2 zero-filled.
        "assert_eq!(func0(), 1.5f32);\n    \
         assert_eq!(func1(), -2.5f32);\n    \
         assert_eq!(func2(), 0.0f32);",
    );
}

#[test]
fn lane_promote() {
    // f64x2.promote_low_f32x4: widen the low two f32 lanes to f64; the upper two
    // source lanes are ignored.
    expect_ok(
        "lane_promote",
        r#"
        (module
          (func (result f64)
            (f64x2.extract_lane 0 (f64x2.promote_low_f32x4 (v128.const f32x4 1.5 -2.5 9 9))))
          (func (result f64)
            (f64x2.extract_lane 1 (f64x2.promote_low_f32x4 (v128.const f32x4 1.5 -2.5 9 9)))))
        "#,
        // 1.5->1.5, -2.5->-2.5 (lane 2/3 ignored).
        "assert_eq!(func0(), 1.5f64);\n    \
         assert_eq!(func1(), -2.5f64);",
    );
}

#[test]
fn lane_abs() {
    // Integer lane abs is wrapping: iN::MIN maps to itself, matching wasm.
    expect_ok(
        "lane_abs",
        r#"
        (module
          (func (result i32)
            (i32x4.extract_lane 0 (i32x4.abs (v128.const i32x4 -5 5 -2147483648 0))))
          (func (result i32)
            (i32x4.extract_lane 2 (i32x4.abs (v128.const i32x4 -5 5 -2147483648 0))))
          (func (result i32)
            (i8x16.extract_lane_s 0
              (i8x16.abs (v128.const i8x16 -5 -128 0 0 0 0 0 0 0 0 0 0 0 0 0 0))))
          (func (result i32)
            (i8x16.extract_lane_s 1
              (i8x16.abs (v128.const i8x16 -5 -128 0 0 0 0 0 0 0 0 0 0 0 0 0 0))))
          (func (result i32)
            (i16x8.extract_lane_s 0 (i16x8.abs (v128.const i16x8 -300 0 0 0 0 0 0 0))))
          (func (result i64)
            (i64x2.extract_lane 0 (i64x2.abs (v128.const i64x2 -9000000000 0)))))
        "#,
        // -5->5, i32::MIN wraps to itself, -128 wraps to itself, -300->300.
        "assert_eq!(func0(), 5);\n    \
         assert_eq!(func1(), i32::MIN);\n    \
         assert_eq!(func2(), 5);\n    \
         assert_eq!(func3(), -128);\n    \
         assert_eq!(func4(), 300);\n    \
         assert_eq!(func5(), 9000000000i64);",
    );
}

#[test]
fn lane_avgr() {
    // avgr_u rounds up: (a + b + 1) >> 1 as unsigned, with no overflow at the top.
    expect_ok(
        "lane_avgr",
        r#"
        (module
          (func (result i32)
            (i8x16.extract_lane_u 0
              (i8x16.avgr_u (v128.const i8x16 3 255 0 0 0 0 0 0 0 0 0 0 0 0 0 0)
                            (v128.const i8x16 4 255 0 0 0 0 0 0 0 0 0 0 0 0 0 0))))
          (func (result i32)
            (i8x16.extract_lane_u 1
              (i8x16.avgr_u (v128.const i8x16 3 255 0 0 0 0 0 0 0 0 0 0 0 0 0 0)
                            (v128.const i8x16 4 255 0 0 0 0 0 0 0 0 0 0 0 0 0 0))))
          (func (result i32)
            (i16x8.extract_lane_u 0
              (i16x8.avgr_u (v128.const i16x8 100 0 0 0 0 0 0 0)
                            (v128.const i16x8 201 0 0 0 0 0 0 0)))))
        "#,
        // (3+4+1)/2=4; (255+255+1)/2=255 (no overflow); (100+201+1)/2=151.
        "assert_eq!(func0(), 4);\n    \
         assert_eq!(func1(), 255);\n    \
         assert_eq!(func2(), 151);",
    );
}

#[test]
fn lane_popcnt() {
    // i8x16.popcnt counts set bits per byte.
    expect_ok(
        "lane_popcnt",
        r#"
        (module
          (func (result i32)
            (i8x16.extract_lane_u 0
              (i8x16.popcnt (v128.const i8x16 0xFF 0x0F 0 0 0 0 0 0 0 0 0 0 0 0 0 0))))
          (func (result i32)
            (i8x16.extract_lane_u 1
              (i8x16.popcnt (v128.const i8x16 0xFF 0x0F 0 0 0 0 0 0 0 0 0 0 0 0 0 0)))))
        "#,
        // popcount(0xFF)=8, popcount(0x0F)=4.
        "assert_eq!(func0(), 8);\n    \
         assert_eq!(func1(), 4);",
    );
}

#[test]
fn lane_all_true() {
    // all_true: 1 if every lane is non-zero, else 0.
    expect_ok(
        "lane_all_true",
        r#"
        (module
          (func (result i32) (i32x4.all_true (v128.const i32x4 1 2 3 4)))
          (func (result i32) (i32x4.all_true (v128.const i32x4 1 0 3 4)))
          (func (result i32)
            (i8x16.all_true (v128.const i8x16 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1)))
          (func (result i32)
            (i8x16.all_true (v128.const i8x16 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 0)))
          (func (result i32) (i64x2.all_true (v128.const i64x2 5 7)))
          (func (result i32) (i64x2.all_true (v128.const i64x2 5 0))))
        "#,
        "assert_eq!(func0(), 1);\n    \
         assert_eq!(func1(), 0);\n    \
         assert_eq!(func2(), 1);\n    \
         assert_eq!(func3(), 0);\n    \
         assert_eq!(func4(), 1);\n    \
         assert_eq!(func5(), 0);",
    );
}

#[test]
fn lane_bitmask() {
    // bitmask gathers the sign bit (MSB) of each lane into the low bits of an i32.
    expect_ok(
        "lane_bitmask",
        r#"
        (module
          (func (result i32)
            (i8x16.bitmask (v128.const i8x16 -1 1 -1 0 0 0 0 0 0 0 0 0 0 0 0 -128)))
          (func (result i32)
            (i32x4.bitmask (v128.const i32x4 -1 0 -2147483648 5)))
          (func (result i32)
            (i16x8.bitmask (v128.const i16x8 -1 0 0 0 0 0 0 -1)))
          (func (result i32)
            (i64x2.bitmask (v128.const i64x2 -1 5))))
        "#,
        // i8x16: bits 0,2,15 -> 1|4|0x8000 = 0x8005; i32x4: bits 0,2 -> 5;
        // i16x8: bits 0,7 -> 1|0x80 = 129; i64x2: bit 0 -> 1.
        "assert_eq!(func0(), 0x8005);\n    \
         assert_eq!(func1(), 5);\n    \
         assert_eq!(func2(), 129);\n    \
         assert_eq!(func3(), 1);",
    );
}
