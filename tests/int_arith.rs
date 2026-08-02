//! Integration tests for Phase 6b: integer division, remainder, shift and
//! rotate for i32 and i64. These carry trap semantics (division/remainder by
//! zero, and `i32::MIN / -1`), so behaviour tests assert results while trap
//! tests assert the generated program panics. All modules are stateless.

use std::process::Command;

fn compile(test: &str, wat: &str, main_body: &str) -> std::path::PathBuf {
    let wasm = wat::parse_str(wat).expect("valid wat");
    let generated = wasm2rs::transpile(&wasm).expect("transpile ok");

    let dir = std::env::temp_dir().join(format!("wasm2rs_intarith_{test}"));
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

fn expect_ok(test: &str, wat: &str, main_body: &str) {
    let bin = compile(test, wat, main_body);
    let run = Command::new(&bin).status().expect("run generated binary");
    assert!(run.success(), "generated program did not succeed: {test}");
}

fn expect_trap(test: &str, wat: &str, main_body: &str) {
    let bin = compile(test, wat, main_body);
    let run = Command::new(&bin).output().expect("run generated binary");
    assert!(
        !run.status.success(),
        "expected a trap but the program exited cleanly: {test}",
    );
}

const I32_OPS: &str = r#"
    (module
      (func (param i32 i32) (result i32) (i32.div_s (local.get 0) (local.get 1)))
      (func (param i32 i32) (result i32) (i32.div_u (local.get 0) (local.get 1)))
      (func (param i32 i32) (result i32) (i32.rem_s (local.get 0) (local.get 1)))
      (func (param i32 i32) (result i32) (i32.rem_u (local.get 0) (local.get 1)))
      (func (param i32 i32) (result i32) (i32.shl (local.get 0) (local.get 1)))
      (func (param i32 i32) (result i32) (i32.shr_s (local.get 0) (local.get 1)))
      (func (param i32 i32) (result i32) (i32.shr_u (local.get 0) (local.get 1)))
      (func (param i32 i32) (result i32) (i32.rotl (local.get 0) (local.get 1)))
      (func (param i32 i32) (result i32) (i32.rotr (local.get 0) (local.get 1))))
    "#;

#[test]
fn i32_div_rem_shift_rotate() {
    expect_ok(
        "i32ops",
        I32_OPS,
        "assert_eq!(func0(-7, 2), -3);\n    \
         assert_eq!(func1(-1, 2), 0x7FFF_FFFF);\n    \
         assert_eq!(func2(-7, 2), -1);\n    \
         assert_eq!(func2(i32::MIN, -1), 0);\n    \
         assert_eq!(func3(7, 3), 1);\n    \
         assert_eq!(func4(1, 33), 2);\n    \
         assert_eq!(func5(-8, 1), -4);\n    \
         assert_eq!(func6(-1, 28), 0xF);\n    \
         assert_eq!(func7(0x1234_5678, 8), 0x3456_7812);\n    \
         assert_eq!(func8(0x1234_5678, 8), 0x7812_3456);",
    );
}

#[test]
fn i32_div_by_zero_traps() {
    expect_trap("i32_divz", I32_OPS, "func0(1, 0);");
}

#[test]
fn i32_div_signed_overflow_traps() {
    expect_trap("i32_divov", I32_OPS, "func0(i32::MIN, -1);");
}

#[test]
fn i32_rem_by_zero_traps() {
    expect_trap("i32_remz", I32_OPS, "func3(1, 0);");
}

#[test]
fn dropped_div_by_zero_still_traps() {
    // The div result is discarded, but wasm evaluates (and traps on) the divide
    // before the drop, so the generated code must still perform the division.
    expect_trap(
        "drop_divz",
        r#"
        (module
          (func (param i32 i32)
            (drop (i32.div_s (local.get 0) (local.get 1)))))
        "#,
        "func0(1, 0);",
    );
}

#[test]
fn i64_div_shift_rotate_widths() {
    expect_ok(
        "i64ops",
        r#"
        (module
          (func (param i64 i64) (result i64) (i64.div_u (local.get 0) (local.get 1)))
          (func (param i64 i64) (result i64) (i64.shr_u (local.get 0) (local.get 1)))
          (func (param i64 i64) (result i64) (i64.rotl (local.get 0) (local.get 1)))
          (func (param i64 i64) (result i64) (i64.rem_s (local.get 0) (local.get 1))))
        "#,
        "assert_eq!(func0(-1, 2), i64::MAX);\n    \
         assert_eq!(func1(-1, 60), 0xF);\n    \
         assert_eq!(func2(0x0123_4567_89AB_CDEF, 8), 0x2345_6789_ABCD_EF01);\n    \
         assert_eq!(func3(i64::MIN, -1), 0);",
    );
}
