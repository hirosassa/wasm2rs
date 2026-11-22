//! Integration tests for Phase 5a: active data segments. A data segment seeds
//! the linear memory at instantiation, so `Instance::new()` must copy the bytes
//! into `memory` before any function runs. Each test compiles the generated
//! Rust with `rustc -D warnings` and reads the bytes back.

use crate::common::compile_run;

#[test]
fn active_data_segment_seeds_memory() {
    // Bytes 01 02 03 04 at offset 0 read back as the little-endian i32
    // 0x04030201.
    compile_run(
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
    compile_run(
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
    compile_run(
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
fn a_mutable_imported_global_data_offset_is_rejected() {
    // An immutable imported global is read from the host at instantiation (see
    // imports.rs); a mutable one is not a constant.
    let err = transpile_err(
        r#"(module
            (import "env" "base" (global $base (mut i32)))
            (memory 1)
            (data (global.get $base) "x"))"#,
    );
    // Input validation (wasmparser) now rejects this before wasm2rs's own check, which is
    // unreachable for validated input; assert the validation rejection instead.
    assert!(err.contains("failed to parse wasm"), "{err}");
    assert!(err.contains("mutable global"), "{err}");
}
