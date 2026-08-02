//! Integration tests for Phase 6d: numeric conversions — integer wrap/extend,
//! sign-extension ops, int<->float conversions, demote/promote, reinterpret,
//! and the saturating float->int truncations. All modules are stateless; each
//! is compiled with `rustc -D warnings` and exercised.

use std::process::Command;

/// Transpile `wat`, wrap the output in a `main` running `main_body`, and compile
/// it with `rustc -D warnings`. Returns the path to the built binary.
fn compile(test: &str, wat: &str, main_body: &str) -> std::path::PathBuf {
    let wasm = wat::parse_str(wat).expect("valid wat");
    let generated = wasm2rs::transpile(&wasm).expect("transpile ok");

    let dir = std::env::temp_dir().join(format!("wasm2rs_convert_{test}"));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let src = dir.join("gen.rs");
    let bin = dir.join(if cfg!(windows) { "gen.exe" } else { "gen" });

    let program = format!("{generated}\nfn main() {{\n{main_body}\n}}\n");
    std::fs::write(&src, &program).expect("write generated source");

    let out = Command::new("rustc")
        // Isolate each parallel rustc's codegen-unit temp objects per test dir.
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

/// Compile `wat` + `main_body` and run it, asserting the program exits
/// successfully (its own `assert!`s hold and it does not trap).
fn transpile_compile_run(test: &str, wat: &str, main_body: &str) {
    let bin = compile(test, wat, main_body);
    let run = Command::new(&bin).status().expect("run generated binary");
    assert!(
        run.success(),
        "generated program assertions failed:\n{test}"
    );
}

#[test]
fn integer_wrap_and_extend() {
    transpile_compile_run(
        "wrap_extend",
        r#"
        (module
          (func (param i64) (result i32) (i32.wrap_i64 (local.get 0)))
          (func (param i32) (result i64) (i64.extend_i32_s (local.get 0)))
          (func (param i32) (result i64) (i64.extend_i32_u (local.get 0)))
          (func (param i32) (result i32) (i32.extend8_s (local.get 0)))
          (func (param i32) (result i32) (i32.extend16_s (local.get 0))))
        "#,
        "assert_eq!(func0(0x1_0000_0007), 7);\n    \
         assert_eq!(func1(-1), -1i64);\n    \
         assert_eq!(func2(-1), 4294967295i64);\n    \
         assert_eq!(func3(0xFF), -1);\n    \
         assert_eq!(func4(0xFFFF), -1);",
    );
}

#[test]
fn int_float_conversions_and_demote_promote() {
    transpile_compile_run(
        "convert",
        r#"
        (module
          (func (param i32) (result f64) (f64.convert_i32_s (local.get 0)))
          (func (param i32) (result f32) (f32.convert_i32_u (local.get 0)))
          (func (param f64) (result f32) (f32.demote_f64 (local.get 0)))
          (func (param f32) (result f64) (f64.promote_f32 (local.get 0))))
        "#,
        "assert_eq!(func0(-5), -5.0f64);\n    \
         assert_eq!(func1(256), 256.0f32);\n    \
         assert_eq!(func2(3.5f64), 3.5f32);\n    \
         assert_eq!(func3(2.25f32), 2.25f64);",
    );
}

#[test]
fn reinterpret_bit_casts() {
    transpile_compile_run(
        "reinterpret",
        r#"
        (module
          (func (param f32) (result i32) (i32.reinterpret_f32 (local.get 0)))
          (func (param i32) (result f32) (f32.reinterpret_i32 (local.get 0))))
        "#,
        "assert_eq!(func0(1.0f32), 0x3F80_0000);\n    \
         assert_eq!(func1(0x3F80_0000), 1.0f32);",
    );
}

#[test]
fn trapping_truncation_in_range() {
    // Non-saturating truncations return the truncated value for in-range inputs.
    transpile_compile_run(
        "trunc_trap_ok",
        r#"
        (module
          (func (param f32) (result i32) (i32.trunc_f32_s (local.get 0)))
          (func (param f32) (result i32) (i32.trunc_f32_u (local.get 0)))
          (func (param f64) (result i32) (i32.trunc_f64_s (local.get 0)))
          (func (param f64) (result i64) (i64.trunc_f64_s (local.get 0)))
          (func (param f32) (result i64) (i64.trunc_f32_u (local.get 0))))
        "#,
        "assert_eq!(func0(3.9f32), 3);\n    \
         assert_eq!(func0(-3.9f32), -3);\n    \
         assert_eq!(func1(3.9f32), 3);\n    \
         assert_eq!(func2(-2.9f64), -2);\n    \
         assert_eq!(func3(-2.9f64), -2i64);\n    \
         assert_eq!(func4(5.5f32), 5i64);",
    );
}

// A signed f32->i32 truncation and an unsigned f64->i64 truncation, used by the
// trap tests below.
const TRUNC_M: &str = r#"
    (module
      (func (param f32) (result i32) (i32.trunc_f32_s (local.get 0)))
      (func (param f64) (result i64) (i64.trunc_f64_u (local.get 0))))
    "#;

#[test]
fn trunc_nan_traps() {
    let bin = compile("trunc_nan", TRUNC_M, "let _ = func0(f32::NAN);");
    let run = Command::new(&bin).output().expect("run generated binary");
    assert!(!run.status.success(), "expected a trap on NaN");
}

#[test]
fn trunc_overflow_traps() {
    let bin = compile("trunc_of", TRUNC_M, "let _ = func0(1e30f32);");
    let run = Command::new(&bin).output().expect("run generated binary");
    assert!(!run.status.success(), "expected a trap on overflow");
}

#[test]
fn trunc_negative_to_unsigned_traps() {
    let bin = compile("trunc_neg", TRUNC_M, "let _ = func1(-1.0f64);");
    let run = Command::new(&bin).output().expect("run generated binary");
    assert!(
        !run.status.success(),
        "expected a trap converting a negative value to unsigned"
    );
}

#[test]
fn trapping_truncation_at_boundaries() {
    // Values right at the in-range edges must pass, exercising the `>=` vs `>`
    // asymmetry between f32 and f64 signed lower bounds.
    transpile_compile_run(
        "trunc_bounds_ok",
        r#"
        (module
          (func (param f64) (result i32) (i32.trunc_f64_s (local.get 0)))
          (func (param f32) (result i32) (i32.trunc_f32_s (local.get 0)))
          (func (param f32) (result i32) (i32.trunc_f32_u (local.get 0))))
        "#,
        // f64 can represent a value between -2^31-1 and -2^31 that truncates to
        // exactly i32::MIN; the f64 signed lower bound is a strict `>`.
        "assert_eq!(func0(-2147483648.5f64), i32::MIN);\n    \
         assert_eq!(func1(-2147483648.0f32), i32::MIN);\n    \
         assert_eq!(func1(2147483520.0f32), 2147483520);\n    \
         assert_eq!(func2(-0.9f32), 0);",
    );
}

// Signed f64->i32 and signed f32->i32, for the boundary trap tests.
const TRUNC_S_M: &str = r#"
    (module
      (func (param f64) (result i32) (i32.trunc_f64_s (local.get 0)))
      (func (param f32) (result i32) (i32.trunc_f32_s (local.get 0))))
    "#;

#[test]
fn trunc_f64_below_i32_min_traps() {
    // -2^31 - 1 truncates below i32::MIN, so the strict `>` lower bound traps.
    let bin = compile(
        "trunc_lo_f64",
        TRUNC_S_M,
        "let _ = func0(-2147483649.0f64);",
    );
    let run = Command::new(&bin).output().expect("run generated binary");
    assert!(!run.status.success(), "expected a trap just below i32::MIN");
}

#[test]
fn trunc_at_2_pow_31_traps() {
    // Exactly 2^31 is one past i32::MAX, so the `< 2^31` upper bound traps.
    let bin = compile("trunc_hi_f32", TRUNC_S_M, "let _ = func1(2147483648.0f32);");
    let run = Command::new(&bin).output().expect("run generated binary");
    assert!(!run.status.success(), "expected a trap at 2^31");
}

#[test]
fn saturating_truncation() {
    transpile_compile_run(
        "trunc_sat",
        r#"
        (module
          (func (param f32) (result i32) (i32.trunc_sat_f32_s (local.get 0)))
          (func (param f32) (result i32) (i32.trunc_sat_f32_u (local.get 0)))
          (func (param f64) (result i64) (i64.trunc_sat_f64_s (local.get 0))))
        "#,
        "assert_eq!(func0(3.9f32), 3);\n    \
         assert_eq!(func0(-3.9f32), -3);\n    \
         assert_eq!(func0(f32::NAN), 0);\n    \
         assert_eq!(func0(1e30f32), i32::MAX);\n    \
         assert_eq!(func1(-1.0f32), 0);\n    \
         assert_eq!(func2(-2.9f64), -2i64);",
    );
}
