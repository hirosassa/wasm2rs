//! Integration tests for the opt-in `unsafe_memory` codegen mode.
//!
//! With `TranspileOptions { unsafe_memory: true }` the memory-access helpers
//! (`r*`/`w*`, `memory.fill`/`memory.copy`, `memory.init`) drop their Rust
//! slice bounds checks and read/write through `get_unchecked`(`_mut`) in an
//! `unsafe` block, gaining speed at the cost of turning an out-of-bounds access
//! into undefined behaviour. The mode is off by default, so the plain
//! [`wasm2rs::transpile`] output must stay byte-for-byte safe.
//!
//! These tests only exercise *in-bounds* accesses: an out-of-bounds access
//! under `unsafe_memory` is UB and must never be executed.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    reason = "test code"
)]

use std::process::Command;
use wasm2rs::TranspileOptions;

/// Transpile `wat` with `unsafe_memory` enabled and return the generated Rust.
fn transpile_unsafe(wat: &str) -> String {
    let wasm = wat::parse_str(wat).expect("valid wat");
    wasm2rs::transpile_with_options(
        &wasm,
        &TranspileOptions {
            unsafe_memory: true,
            ..Default::default()
        },
    )
    .expect("transpile ok")
}

/// Compile `generated` (wrapped in a `main` running `main_body`) with `rustc -D
/// warnings`, run it, and assert both the build and the run succeed. `test` names
/// a per-invocation temp directory so parallel `rustc`s do not collide.
fn compile_and_run(test: &str, generated: &str, main_body: &str) {
    let dir = std::env::temp_dir().join(format!("wasm2rs_unsafe_mem_{test}"));
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
    let run = Command::new(&bin).status().expect("run generated binary");
    assert!(run.success(), "generated program did not succeed: {test}");
}

/// Compile the `unsafe_memory` output of `wat` (running `main_body`) and assert
/// it succeeds.
fn run_unsafe(test: &str, wat: &str, main_body: &str) {
    compile_and_run(test, &transpile_unsafe(wat), main_body);
}

/// Run `main_body` against both the default (safe) and the `unsafe_memory`
/// lowering of `wat`. `main_body` asserts exact results, so passing under both
/// proves the unchecked path reproduces the safe path's in-bounds behaviour.
fn run_safe_and_unsafe(test: &str, wat: &str, main_body: &str) {
    let wasm = wat::parse_str(wat).expect("valid wat");
    let safe = wasm2rs::transpile(&wasm).expect("transpile ok");
    compile_and_run(&format!("{test}_safe"), &safe, main_body);
    compile_and_run(&format!("{test}_unsafe"), &transpile_unsafe(wat), main_body);
}

const LOAD_STORE: &str = r#"
    (module
      (memory 1)
      (func $store (param i32 i32) (i32.store (local.get 0) (local.get 1)))
      (func $load (param i32) (result i32) (i32.load (local.get 0)))
      (func $store8 (param i32 i32) (i32.store8 (local.get 0) (local.get 1)))
      (func $load8u (param i32) (result i32) (i32.load8_u (local.get 0))))
    "#;

#[test]
fn unsafe_memory_emits_unchecked_access() {
    // The scalar load/store helpers must drop the slice bounds check: their
    // bodies read/write through `get_unchecked`(`_mut`) inside an `unsafe`
    // block, and are marked `#[inline(always)]`.
    let src = transpile_unsafe(LOAD_STORE);
    assert!(
        src.contains("unsafe") && src.contains("get_unchecked"),
        "unsafe_memory must emit unchecked memory access:\n{src}",
    );
    assert!(
        src.contains("#[inline(always)]"),
        "unsafe_memory helpers should be force-inlined:\n{src}",
    );
}

#[test]
fn unsafe_memory_load_store_roundtrip() {
    // In-bounds store then load returns the stored value (32-bit and 8-bit).
    // $store=func0, $load=func1, $store8=func2, $load8u=func3.
    run_unsafe(
        "load_store",
        LOAD_STORE,
        "let mut inst = Instance::new();\n    \
         inst.func0(16, 12345);\n    \
         assert_eq!(inst.func1(16), 12345);\n    \
         inst.func2(40, 0xAB);\n    \
         assert_eq!(inst.func3(40), 0xAB);",
    );
}

#[test]
fn unsafe_memory_bulk_copy_and_fill() {
    // memory.fill then memory.copy over in-bounds ranges behave normally.
    // $fill=func0, $copy=func1, $load=func2.
    run_unsafe(
        "bulk",
        r#"
        (module
          (memory 1)
          (func $fill (param i32 i32 i32)
            (memory.fill (local.get 0) (local.get 1) (local.get 2)))
          (func $copy (param i32 i32 i32)
            (memory.copy (local.get 0) (local.get 1) (local.get 2)))
          (func $load (param i32) (result i32) (i32.load8_u (local.get 0))))
        "#,
        "let mut inst = Instance::new();\n    \
         inst.func0(0, 0x7, 8);\n    \
         assert_eq!(inst.func2(3), 0x7);\n    \
         inst.func1(64, 0, 8);\n    \
         assert_eq!(inst.func2(67), 0x7);",
    );
}

#[test]
fn unsafe_memory_init_from_passive_segment() {
    // memory.init copies from a passive data segment into memory; in-bounds.
    // $init=func0, $load=func1.
    run_unsafe(
        "init",
        r#"
        (module
          (memory 1)
          (data $d "wasm2rs!")
          (func $init (param i32 i32 i32)
            (memory.init $d (local.get 0) (local.get 1) (local.get 2)))
          (func $load (param i32) (result i32) (i32.load8_u (local.get 0))))
        "#,
        "let mut inst = Instance::new();\n    \
         inst.func0(32, 0, 8);\n    \
         assert_eq!(inst.func1(32), b'w' as i32);\n    \
         assert_eq!(inst.func1(39), b'!' as i32);",
    );
}

#[test]
fn unsafe_memory_simd_lane_helpers_match_safe() {
    // Every memory-touching SIMD helper (v128 load/store, load*_splat, load*_lane,
    // store*_lane, load*_zero, load*x*_u widening) must produce the same in-bounds
    // result unchecked as it does bounds-checked. Each function reads a lane back
    // as an i32; the shared `main_body` asserts exact values, run under both
    // lowerings. All addresses stay within the single 64 KiB page.
    let wat = r#"
        (module
          (memory 1)
          (func $vroundtrip (result i32)
            (v128.store (i32.const 16) (v128.const i32x4 111 222 333 444))
            (i32x4.extract_lane 2 (v128.load (i32.const 16))))
          (func $splat32 (result i32)
            (i32.store (i32.const 32) (i32.const 7))
            (i32x4.extract_lane 3 (v128.load32_splat (i32.const 32))))
          (func $lane8 (result i32)
            (i32.store8 (i32.const 48) (i32.const 0x2A))
            (i8x16.extract_lane_u 5
              (v128.load8_lane 5 (i32.const 48) (v128.const i32x4 0 0 0 0))))
          (func $storelane8 (result i32)
            (v128.store8_lane 3 (i32.const 64) (i8x16.splat (i32.const 0x5B)))
            (i32.load8_u (i32.const 64)))
          (func $load32zero (result i32)
            (i32.store (i32.const 80) (i32.const 999))
            (i32x4.extract_lane 0 (v128.load32_zero (i32.const 80))))
          (func $widen8x8 (result i32)
            (i32.store (i32.const 96) (i32.const 0x04030201))
            (i32.store (i32.const 100) (i32.const 0x08070605))
            (i16x8.extract_lane_u 2 (v128.load8x8_u (i32.const 96)))))
    "#;
    run_safe_and_unsafe(
        "simd_lane",
        wat,
        "let mut inst = Instance::new();\n    \
         assert_eq!(inst.func0(), 333);\n    \
         assert_eq!(inst.func1(), 7);\n    \
         assert_eq!(inst.func2(), 0x2A);\n    \
         assert_eq!(inst.func3(), 0x5B);\n    \
         assert_eq!(inst.func4(), 999);\n    \
         assert_eq!(inst.func5(), 3);",
    );
}

#[test]
fn default_transpile_stays_safe() {
    // The default (safe) path must not emit any unchecked memory access, so the
    // existing golden output and wasm trap semantics are preserved.
    let wasm = wat::parse_str(LOAD_STORE).expect("valid wat");
    let src = wasm2rs::transpile(&wasm).expect("transpile ok");
    assert!(
        !src.contains("get_unchecked"),
        "default transpile must stay bounds-checked (safe):\n{src}",
    );
}
