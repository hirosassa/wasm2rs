//! Integration tests for Phase 6d: numeric conversions — integer wrap/extend,
//! sign-extension ops, int<->float conversions, demote/promote, reinterpret,
//! and the saturating float->int truncations. All modules are stateless; each
//! is compiled with `rustc -D warnings` and exercised.

use std::process::Command;

fn transpile_compile_run(test: &str, wat: &str, main_body: &str) {
    let wasm = wat::parse_str(wat).expect("valid wat");
    let generated = wasm2rs::transpile(&wasm).expect("transpile ok");

    let dir = std::env::temp_dir().join(format!("wasm2rs_convert_{test}"));
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
