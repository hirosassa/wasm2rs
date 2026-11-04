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
