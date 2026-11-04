//! Integration tests for Phase 5a: active data segments. A data segment seeds
//! the linear memory at instantiation, so `Instance::new()` must copy the bytes
//! into `memory` before any function runs. Each test compiles the generated
//! Rust with `rustc -D warnings` and reads the bytes back.

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

fn transpile_compile_run(test: &str, wat: &str, main_body: &str) {
    let wasm = wat::parse_str(wat).expect("valid wat");
    let generated = wasm2rs::transpile(&wasm).expect("transpile ok");

    let dir = std::env::temp_dir().join(format!("wasm2rs_data_{test}"));
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
        .arg("2024")
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
fn active_data_segment_seeds_memory() {
    // Bytes 01 02 03 04 at offset 0 read back as the little-endian i32
    // 0x04030201.
    transpile_compile_run(
        "seed",
        r#"
        (module
          (memory 1)
          (data (i32.const 0) "\01\02\03\04")
          (func (param i32) (result i32) (i32.load (local.get 0))))
        "#,
        "let mut inst = Instance::new();\n    \
         assert_eq!(inst.func0(0), 0x04030201);",
    );
}

#[test]
fn multiple_segments_and_offsets() {
    transpile_compile_run(
        "offsets",
        r#"
        (module
          (memory 1)
          (data (i32.const 4) "\aa")
          (data (i32.const 16) "\ff\ff")
          (func (param i32) (result i32) (i32.load8_u (local.get 0))))
        "#,
        "let mut inst = Instance::new();\n    \
         assert_eq!(inst.func0(4), 0xAA);\n    \
         assert_eq!(inst.func0(16), 0xFF);\n    \
         assert_eq!(inst.func0(0), 0);",
    );
}

#[test]
fn active_data_segment_round_trips_bytes_that_need_escaping() {
    // 64 filler bytes (one full wrapped line) put a space at the start of the
    // second line, where a `\` continuation would swallow it if unescaped; the
    // rest cover quote, backslash, whitespace controls, NUL and high bytes.
    let filler = "A".repeat(64);
    transpile_compile_run(
        "escaping",
        &format!(
            r#"
        (module
          (memory 1)
          (data (i32.const 0) "{filler} \22\5c\0a\0d\09\00\ff\7f~")
          (func (param i32) (result i32) (i32.load8_u (local.get 0))))
        "#
        ),
        "let mut inst = Instance::new();\n    \
         let expect: [u8; 9] = [0x22, 0x5c, 0x0a, 0x0d, 0x09, 0x00, 0xff, 0x7f, 0x7e];\n    \
         assert_eq!(inst.func0(63), 0x41);\n    \
         assert_eq!(inst.func0(64), 0x20);\n    \
         for (i, b) in expect.iter().enumerate() {\n        \
             assert_eq!(inst.func0(65 + i as i32), *b as i32, \"byte {i}\");\n    \
         }",
    );
}

fn transpile_err(wat: &str) -> String {
    let wasm = wat::parse_str(wat).expect("valid wat");
    wasm2rs::transpile(&wasm)
        .expect_err("transpile should be rejected")
        .to_string()
}

#[test]
fn a_negative_i64_data_offset_is_rejected() {
    let err = transpile_err(r#"(module (memory i64 1) (data (i64.const -1) "x"))"#);
    assert!(err.contains("negative data offset"), "{err}");
}

#[test]
fn a_global_derived_data_offset_is_rejected() {
    let err = transpile_err(
        r#"(module
            (import "env" "base" (global $base i32))
            (memory 1)
            (data (global.get $base) "x"))"#,
    );
    assert!(err.contains("data offset"), "{err}");
}
