//! Integration tests for the bit-counting unary ops `clz`/`ctz`/`popcnt`
//! (i32 and i64). These are the operators a real `wasm32-wasip1` binary pulls
//! in (Rust's core uses `ctz`/`clz` for integer formatting and slice length
//! maths), so covering them lets a full std hello-world transpile. Rust's
//! `leading_zeros`/`trailing_zeros`/`count_ones` match wasm's semantics exactly,
//! including `clz(0) == N` and `ctz(0) == N`. All modules are stateless.

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

    let dir = std::env::temp_dir().join(format!("wasm2rs_bitcount_{test}"));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let src = dir.join("gen.rs");
    let bin = dir.join(if cfg!(windows) { "gen.exe" } else { "gen" });

    let program = format!("{generated}\nfn main() {{\n{main_body}\n}}\n");
    std::fs::write(&src, &program).expect("write generated source");

    let out = Command::new("rustc")
        .current_dir(&dir)
        .arg(&src)
        .arg("--edition")
        .arg("2024")
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
fn i32_clz_ctz_popcnt() {
    expect_ok(
        "i32bits",
        r#"
        (module
          (func (param i32) (result i32) (i32.clz (local.get 0)))
          (func (param i32) (result i32) (i32.ctz (local.get 0)))
          (func (param i32) (result i32) (i32.popcnt (local.get 0))))
        "#,
        "assert_eq!(func0(0), 32);\n    \
         assert_eq!(func0(1), 31);\n    \
         assert_eq!(func0(i32::MIN), 0);\n    \
         assert_eq!(func1(0), 32);\n    \
         assert_eq!(func1(8), 3);\n    \
         assert_eq!(func1(i32::MIN), 31);\n    \
         assert_eq!(func2(0), 0);\n    \
         assert_eq!(func2(-1), 32);\n    \
         assert_eq!(func2(0xF0F0), 8);",
    );
}

#[test]
fn i64_clz_ctz_popcnt() {
    expect_ok(
        "i64bits",
        r#"
        (module
          (func (param i64) (result i64) (i64.clz (local.get 0)))
          (func (param i64) (result i64) (i64.ctz (local.get 0)))
          (func (param i64) (result i64) (i64.popcnt (local.get 0))))
        "#,
        "assert_eq!(func0(0), 64);\n    \
         assert_eq!(func0(1), 63);\n    \
         assert_eq!(func1(0), 64);\n    \
         assert_eq!(func1(8), 3);\n    \
         assert_eq!(func2(0), 0);\n    \
         assert_eq!(func2(-1), 64);\n    \
         assert_eq!(func2(0xF0F0), 8);",
    );
}
