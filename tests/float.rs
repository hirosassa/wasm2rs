//! Integration tests for Phase 6c: f32/f64 operators (const, arithmetic,
//! comparisons with NaN handling, and unary math). These modules are
//! stateless, so the transpiler emits free functions; each is compiled with
//! `rustc -D warnings` and exercised.

use std::process::Command;

fn transpile_compile_run(test: &str, wat: &str, main_body: &str) {
    let wasm = wat::parse_str(wat).expect("valid wat");
    let generated = wasm2rs::transpile(&wasm).expect("transpile ok");

    let dir = std::env::temp_dir().join(format!("wasm2rs_float_{test}"));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let src = dir.join("gen.rs");
    let bin = dir.join(if cfg!(windows) { "gen.exe" } else { "gen" });

    let program = format!("{generated}\nfn main() {{\n{main_body}\n}}\n");
    std::fs::write(&src, &program).expect("write generated source");

    let compile = Command::new("rustc")
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
        compile.status.success(),
        "generated code failed to compile:\n{program}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&compile.stderr),
    );

    let run = Command::new(&bin).status().expect("run generated binary");
    assert!(
        run.success(),
        "generated program assertions failed:\n{program}"
    );
}

#[test]
fn float_arithmetic() {
    transpile_compile_run(
        "arith",
        r#"
        (module
          (func (param f32 f32) (result f32) (f32.add (local.get 0) (local.get 1)))
          (func (param f32 f32) (result f32) (f32.sub (local.get 0) (local.get 1)))
          (func (param f32 f32) (result f32) (f32.div (local.get 0) (local.get 1)))
          (func (param f64 f64) (result f64) (f64.mul (local.get 0) (local.get 1))))
        "#,
        "assert_eq!(func0(1.5, 2.25), 3.75);\n    \
         assert_eq!(func1(5.0, 1.25), 3.75);\n    \
         assert!(func2(1.0, 0.0).is_infinite());\n    \
         assert_eq!(func3(2.0, 3.0), 6.0);",
    );
}

#[test]
fn float_comparisons_with_nan() {
    transpile_compile_run(
        "cmp",
        r#"
        (module
          (func (param f32 f32) (result i32) (f32.lt (local.get 0) (local.get 1)))
          (func (param f32 f32) (result i32) (f32.eq (local.get 0) (local.get 1)))
          (func (param f64 f64) (result i32) (f64.ge (local.get 0) (local.get 1))))
        "#,
        "assert_eq!(func0(1.0, 2.0), 1);\n    \
         assert_eq!(func0(2.0, 1.0), 0);\n    \
         assert_eq!(func0(f32::NAN, 1.0), 0);\n    \
         assert_eq!(func1(3.0, 3.0), 1);\n    \
         assert_eq!(func1(f32::NAN, f32::NAN), 0);\n    \
         assert_eq!(func2(5.0, 5.0), 1);",
    );
}

#[test]
fn float_min_max() {
    // wasm min/max propagate NaN and treat -0.0 as strictly less than +0.0,
    // unlike Rust's `f32::min`/`max`, so they need dedicated helpers.
    transpile_compile_run(
        "minmax",
        r#"
        (module
          (func (param f32 f32) (result f32) (f32.min (local.get 0) (local.get 1)))
          (func (param f32 f32) (result f32) (f32.max (local.get 0) (local.get 1)))
          (func (param f64 f64) (result f64) (f64.min (local.get 0) (local.get 1)))
          (func (param f64 f64) (result f64) (f64.max (local.get 0) (local.get 1))))
        "#,
        "assert_eq!(func0(1.0f32, 2.0), 1.0);\n    \
         assert_eq!(func1(1.0f32, 2.0), 2.0);\n    \
         assert!(func0(f32::NAN, 1.0).is_nan());\n    \
         assert!(func1(1.0f32, f32::NAN).is_nan());\n    \
         assert!(func0(0.0f32, -0.0).is_sign_negative());\n    \
         assert!(func1(0.0f32, -0.0).is_sign_positive());\n    \
         assert_eq!(func2(3.0f64, -5.0), -5.0);\n    \
         assert_eq!(func3(3.0f64, -5.0), 3.0);\n    \
         assert!(func2(-0.0f64, 0.0).is_sign_negative());\n    \
         assert!(func3(-0.0f64, 0.0).is_sign_positive());\n    \
         assert!(func2(f64::NAN, 1.0).is_nan());",
    );
}

#[test]
fn float_unary_math_and_copysign() {
    transpile_compile_run(
        "unary",
        r#"
        (module
          (func (param f64) (result f64) (f64.sqrt (local.get 0)))
          (func (param f32) (result f32) (f32.abs (local.get 0)))
          (func (param f32) (result f32) (f32.neg (local.get 0)))
          (func (param f64) (result f64) (f64.floor (local.get 0)))
          (func (param f64) (result f64) (f64.nearest (local.get 0)))
          (func (param f32 f32) (result f32) (f32.copysign (local.get 0) (local.get 1))))
        "#,
        "assert_eq!(func0(9.0), 3.0);\n    \
         assert_eq!(func1(-5.0), 5.0);\n    \
         assert_eq!(func2(3.0), -3.0);\n    \
         assert_eq!(func3(2.7), 2.0);\n    \
         assert_eq!(func4(2.5), 2.0);\n    \
         assert_eq!(func4(3.5), 4.0);\n    \
         assert_eq!(func5(3.0, -1.0), -3.0);",
    );
}

#[test]
fn float_const_is_emitted_from_bits() {
    transpile_compile_run(
        "const",
        r#"
        (module
          (func (result f32) (f32.const 1.5))
          (func (result f64) (f64.const 2.5)))
        "#,
        "assert_eq!(func0(), 1.5);\n    assert_eq!(func1(), 2.5);",
    );
}
