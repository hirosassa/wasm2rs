//! Integration tests for Phase 6e: the `unreachable` operator, which always
//! traps. A reached `unreachable` must panic; code after it is dead. Each
//! module is compiled with `rustc -D warnings`; the behaviour test asserts a
//! live path returns normally, the trap tests assert the program panics.

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

    let dir = std::env::temp_dir().join(format!("wasm2rs_unreach_{test}"));
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

const M: &str = r#"
    (module
      (func (result i32) (unreachable))
      (func (param i32) (result i32)
        (if (result i32) (local.get 0)
          (then (i32.const 42))
          (else (unreachable)))))
    "#;

#[test]
fn live_path_returns_normally() {
    let bin = compile("live", M, "assert_eq!(func1(1), 42);");
    let run = std::process::Command::new(&bin)
        .status()
        .expect("run generated binary");
    assert!(run.success(), "expected normal exit");
}

#[test]
fn bare_unreachable_traps() {
    let bin = compile("bare", M, "func0();");
    let run = std::process::Command::new(&bin)
        .output()
        .expect("run generated binary");
    assert!(!run.status.success(), "expected a trap");
}

#[test]
fn unreachable_in_branch_traps() {
    let bin = compile("branch", M, "func1(0);");
    let run = std::process::Command::new(&bin)
        .output()
        .expect("run generated binary");
    assert!(!run.status.success(), "expected a trap");
}
