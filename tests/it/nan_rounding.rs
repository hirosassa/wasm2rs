//! `floor`/`ceil`/`trunc`/`nearest` must return a *quiet* NaN for a NaN input.
//! Without hardware rounding instructions (baseline x86_64 Linux) the host's
//! implementation returns a signaling NaN unchanged, which the spec forbids.

use crate::common;

const OPS: [&str; 4] = ["floor", "ceil", "trunc", "nearest"];

fn module(ty: &str) -> String {
    let funcs: String = OPS
        .iter()
        .map(|op| {
            format!(r#"(func (export "{op}") (param {ty}) (result {ty}) local.get 0 {ty}.{op})"#)
        })
        .collect();
    format!("(module {funcs})")
}

#[test]
fn rounding_results_are_routed_through_a_quieting_helper() {
    for ty in ["f32", "f64"] {
        let wasm = wat::parse_str(module(ty)).unwrap();
        let out = wasm2rs::transpile(&wasm).unwrap();
        assert!(
            out.contains(&format!("fn quiet_{ty}(")),
            "{ty}: missing helper:\n{out}"
        );
        assert_eq!(
            out.matches(&format!("quiet_{ty}((")).count(),
            OPS.len(),
            "{ty}: every rounding op should be wrapped:\n{out}"
        );
    }
}

#[test]
fn the_helper_is_not_emitted_without_rounding_ops() {
    let wasm = wat::parse_str(
        r#"(module (func (export "f") (param f32) (result f32) local.get 0 f32.sqrt))"#,
    )
    .unwrap();
    let out = wasm2rs::transpile(&wasm).unwrap();
    assert!(!out.contains("quiet_f32"), "unexpected helper:\n{out}");
}

#[test]
fn a_signaling_nan_input_gives_a_quiet_nan() {
    // Indices follow the export order: floor, ceil, trunc, nearest.
    common::compile_run(
        "nan_rounding_f32",
        &module("f32"),
        "let snan = f32::from_bits(0xFFA0_0000);\n \
         for r in [func0(snan), func1(snan), func2(snan), func3(snan)] {\n \
             assert!(r.is_nan() && r.to_bits() & 0x0040_0000 != 0, \"{:#x}\", r.to_bits());\n \
         }",
    );
    common::compile_run(
        "nan_rounding_f64",
        &module("f64"),
        "let snan = f64::from_bits(0xFFF4_0000_0000_0000);\n \
         for r in [func0(snan), func1(snan), func2(snan), func3(snan)] {\n \
             assert!(r.is_nan() && r.to_bits() & 0x0008_0000_0000_0000 != 0, \"{:#x}\", r.to_bits());\n \
         }",
    );
}

#[test]
fn ordinary_values_are_unchanged() {
    common::compile_run(
        "nan_rounding_values",
        &module("f64"),
        "assert_eq!(func0(-1.5), -2.0);\n \
         assert_eq!(func1(1.2), 2.0);\n \
         assert_eq!(func2(-1.9), -1.0);\n \
         assert_eq!(func3(2.5), 2.0);\n \
         assert_eq!(func3(3.5), 4.0);\n \
         assert_eq!(func0(-0.0).to_bits(), (-0.0f64).to_bits());\n \
         assert_eq!(func2(f64::INFINITY), f64::INFINITY);",
    );
}

const CONVERSIONS: &str = r#"(module
  (func (export "demote") (param f64) (result f32) local.get 0 f32.demote_f64)
  (func (export "promote") (param f32) (result f64) local.get 0 f64.promote_f32)
  (func (export "round_trip") (param f32) (result f32)
    local.get 0 f64.promote_f32 f32.demote_f64))"#;

#[test]
fn demote_and_promote_results_are_routed_through_a_quieting_helper() {
    // LLVM folds `(x as f64) as f32` to `x`, which would leak a signaling NaN, so
    // the conversion itself must quiet a NaN result rather than rely on hardware.
    let wasm = wat::parse_str(CONVERSIONS).unwrap();
    let out = wasm2rs::transpile(&wasm).unwrap();
    assert!(
        out.contains("quiet_f32(l0 as f32)"),
        "demote not quieted:\n{out}"
    );
    assert!(
        out.contains("quiet_f64(l0 as f64)"),
        "promote not quieted:\n{out}"
    );
}

#[test]
fn demote_and_promote_of_a_signaling_nan_give_a_quiet_nan() {
    // Indices follow the export order: demote, promote, round_trip.
    common::compile_run(
        "nan_conversions",
        CONVERSIONS,
        "let d = func0(f64::from_bits(0x7FF4_0000_0000_0000));\n \
         assert!(d.is_nan() && d.to_bits() & 0x0040_0000 != 0, \"{:#x}\", d.to_bits());\n \
         let p = func1(f32::from_bits(0x7FA0_0000));\n \
         assert!(p.is_nan() && p.to_bits() & 0x0008_0000_0000_0000 != 0, \"{:#x}\", p.to_bits());\n \
         let r = func2(f32::from_bits(0x7FA0_0000));\n \
         assert!(r.is_nan() && r.to_bits() & 0x0040_0000 != 0, \"{:#x}\", r.to_bits());\n \
         assert_eq!(func2(1.5), 1.5);\n \
         assert_eq!(func0(f64::INFINITY), f32::INFINITY);\n \
         assert_eq!(func1(-0.0).to_bits(), (-0.0f64).to_bits());",
    );
}

#[test]
fn simd_demote_and_promote_of_a_signaling_nan_give_a_quiet_nan() {
    common::compile_run(
        "nan_simd_conversions",
        r#"(module
          (func (export "demote") (param v128) (result v128) local.get 0 f32x4.demote_f64x2_zero)
          (func (export "promote") (param v128) (result v128) local.get 0 f64x2.promote_low_f32x4))"#,
        "let d = func0(0x7FF4_0000_0000_0000u128).to_le_bytes();\n \
         let d = u32::from_le_bytes(d[..4].try_into().unwrap());\n \
         assert!(f32::from_bits(d).is_nan() && d & 0x0040_0000 != 0, \"{d:#x}\");\n \
         let p = func1(0x7FA0_0000u128).to_le_bytes();\n \
         let p = u64::from_le_bytes(p[..8].try_into().unwrap());\n \
         assert!(f64::from_bits(p).is_nan() && p & 0x0008_0000_0000_0000 != 0, \"{p:#x}\");",
    );
}
