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
