//! Integration tests for Phase 3: linear memory and globals. Each test
//! transpiles a stateful module (which becomes a `struct Instance`), compiles
//! the generated Rust with `rustc -D warnings`, and runs assertions against an
//! `Instance` so the memory/global behaviour is verified end to end.

use std::process::Command;

fn transpile_compile_run(test: &str, wat: &str, main_body: &str) {
    let wasm = wat::parse_str(wat).expect("valid wat");
    let generated = wasm2rs::transpile(&wasm).expect("transpile ok");

    let dir = std::env::temp_dir().join(format!("wasm2rs_state_{test}"));
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
fn mutable_global_accumulates() {
    transpile_compile_run(
        "global",
        r#"
        (module
          (global (mut i32) (i32.const 10))
          (func (result i32)
            (global.set 0 (i32.add (global.get 0) (i32.const 5)))
            (global.get 0)))
        "#,
        "let mut inst = Instance::new();\n    \
         assert_eq!(inst.func0(), 15);\n    \
         assert_eq!(inst.func0(), 20);",
    );
}

#[test]
fn i32_store_load_roundtrip() {
    transpile_compile_run(
        "mem_i32",
        r#"
        (module
          (memory 1)
          (func (param i32 i32) (i32.store (local.get 0) (local.get 1)))
          (func (param i32) (result i32) (i32.load (local.get 0))))
        "#,
        "let mut inst = Instance::new();\n    \
         inst.func0(4, 0x0BADF00Du32 as i32);\n    \
         assert_eq!(inst.func1(4), 0x0BADF00Du32 as i32);\n    \
         assert_eq!(inst.func1(0), 0);",
    );
}

#[test]
fn narrow_stores_and_sign_extension() {
    transpile_compile_run(
        "mem_narrow",
        r#"
        (module
          (memory 1)
          (func (param i32 i32) (i32.store8 (local.get 0) (local.get 1)))
          (func (param i32 i32) (i32.store16 (local.get 0) (local.get 1)))
          (func (param i32) (result i32) (i32.load8_s (local.get 0)))
          (func (param i32) (result i32) (i32.load8_u (local.get 0)))
          (func (param i32) (result i32) (i32.load16_s (local.get 0)))
          (func (param i32) (result i32) (i32.load16_u (local.get 0))))
        "#,
        "let mut inst = Instance::new();\n    \
         inst.func0(0, 0xFF);\n    \
         assert_eq!(inst.func2(0), -1);\n    \
         assert_eq!(inst.func3(0), 255);\n    \
         inst.func1(2, 0xFFFF);\n    \
         assert_eq!(inst.func4(2), -1);\n    \
         assert_eq!(inst.func5(2), 65535);",
    );
}

#[test]
fn i64_store_load_roundtrip() {
    transpile_compile_run(
        "mem_i64",
        r#"
        (module
          (memory 1)
          (func (param i32 i64) (i64.store (local.get 0) (local.get 1)))
          (func (param i32) (result i64) (i64.load (local.get 0))))
        "#,
        "let mut inst = Instance::new();\n    \
         inst.func0(8, 0x0123456789ABCDEFu64 as i64);\n    \
         assert_eq!(inst.func1(8), 0x0123456789ABCDEFu64 as i64);\n    \
         assert_eq!(inst.func1(0), 0);",
    );
}

#[test]
fn i64_negative_roundtrip_at_offset() {
    // A negative value exercises the sign bit through to_le_bytes/from_le_bytes,
    // and a non-zero memarg offset is folded into the effective address.
    transpile_compile_run(
        "mem_i64_offset",
        r#"
        (module
          (memory 1)
          (func (param i32 i64) (i64.store offset=32 (local.get 0) (local.get 1)))
          (func (param i32) (result i64) (i64.load offset=32 (local.get 0))))
        "#,
        "let mut inst = Instance::new();\n    \
         inst.func0(8, -12345678901234i64);\n    \
         assert_eq!(inst.func1(8), -12345678901234i64);",
    );
}

#[test]
fn float_store_load_roundtrip() {
    transpile_compile_run(
        "mem_float",
        r#"
        (module
          (memory 1)
          (func (param i32 f32) (f32.store (local.get 0) (local.get 1)))
          (func (param i32) (result f32) (f32.load (local.get 0)))
          (func (param i32 f64) (f64.store (local.get 0) (local.get 1)))
          (func (param i32) (result f64) (f64.load (local.get 0))))
        "#,
        "let mut inst = Instance::new();\n    \
         inst.func0(0, 1.5f32);\n    \
         assert_eq!(inst.func1(0), 1.5f32);\n    \
         inst.func2(8, 2.25f64);\n    \
         assert_eq!(inst.func3(8), 2.25f64);",
    );
}

#[test]
fn i64_narrow_stores_and_sign_extension() {
    transpile_compile_run(
        "mem_i64_narrow",
        r#"
        (module
          (memory 1)
          (func (param i32 i64) (i64.store8 (local.get 0) (local.get 1)))
          (func (param i32 i64) (i64.store16 (local.get 0) (local.get 1)))
          (func (param i32 i64) (i64.store32 (local.get 0) (local.get 1)))
          (func (param i32) (result i64) (i64.load8_s (local.get 0)))
          (func (param i32) (result i64) (i64.load8_u (local.get 0)))
          (func (param i32) (result i64) (i64.load16_s (local.get 0)))
          (func (param i32) (result i64) (i64.load16_u (local.get 0)))
          (func (param i32) (result i64) (i64.load32_s (local.get 0)))
          (func (param i32) (result i64) (i64.load32_u (local.get 0))))
        "#,
        "let mut inst = Instance::new();\n    \
         inst.func0(0, 0xFF);\n    \
         assert_eq!(inst.func3(0), -1);\n    \
         assert_eq!(inst.func4(0), 255);\n    \
         inst.func1(2, 0xFFFF);\n    \
         assert_eq!(inst.func5(2), -1);\n    \
         assert_eq!(inst.func6(2), 65535);\n    \
         inst.func2(4, 0xFFFFFFFFi64);\n    \
         assert_eq!(inst.func7(4), -1);\n    \
         assert_eq!(inst.func8(4), 4294967295);",
    );
}

#[test]
fn rt_helper_prepended_before_instance() {
    // A stateful module (has memory) becomes a `struct Instance`; a runtime
    // free-function helper like `f32_min` must still be emitted at module scope
    // ahead of the struct so the generated code compiles and runs.
    transpile_compile_run(
        "rt_stateful",
        r#"
        (module
          (memory 1)
          (func (param f32 f32) (result f32) (f32.min (local.get 0) (local.get 1))))
        "#,
        "let mut inst = Instance::new();\n    \
         assert_eq!(inst.func0(1.0f32, 2.0), 1.0);\n    \
         assert!(inst.func0(f32::NAN, 2.0).is_nan());",
    );
}

#[test]
fn memory_size_and_grow() {
    transpile_compile_run(
        "mem_grow",
        r#"
        (module
          (memory 1)
          (func (result i32) (memory.size))
          (func (param i32) (result i32) (memory.grow (local.get 0))))
        "#,
        "let mut inst = Instance::new();\n    \
         assert_eq!(inst.func0(), 1);\n    \
         assert_eq!(inst.func1(2), 1);\n    \
         assert_eq!(inst.func0(), 3);\n    \
         assert_eq!(inst.func1(65536), -1);\n    \
         assert_eq!(inst.func0(), 3);",
    );
}

/// A module with one memory of the given limits (e.g. `"1 2"`) and `grow`
/// (func0), `size` (func1), `store8` (func2) and `load8_u` (func3).
fn grow_size_wat(limits: &str) -> String {
    format!(
        r#"(module
          (memory {limits})
          (func (param i32) (result i32) (memory.grow (local.get 0)))
          (func (result i32) (memory.size))
          (func (param i32 i32) (i32.store8 (local.get 0) (local.get 1)))
          (func (param i32) (result i32) (i32.load8_u (local.get 0))))"#
    )
}

#[test]
fn memory_grow_past_declared_maximum_fails_and_preserves_size() {
    // `(memory 1 2)`: growing by two would reach three pages, past the declared
    // maximum, so it returns -1; the size and the existing bytes are untouched.
    // A negative operand is a huge unsigned page count and fails too.
    transpile_compile_run(
        "mem_grow_past_max",
        &grow_size_wat("1 2"),
        "let mut inst = Instance::new();\n    \
         inst.func2(0, 0x5A);\n    \
         assert_eq!(inst.func0(2), -1);\n    \
         assert_eq!(inst.func1(), 1);\n    \
         assert_eq!(inst.func3(0), 0x5A);\n    \
         assert_eq!(inst.func0(-1), -1);\n    \
         assert_eq!(inst.func1(), 1);",
    );
}

#[test]
fn memory_grow_to_exactly_the_declared_maximum_succeeds() {
    // Growing to exactly two pages is allowed; one more page fails; growing by
    // zero (also at the maximum) returns the current size and changes nothing.
    transpile_compile_run(
        "mem_grow_exact_max",
        &grow_size_wat("1 2"),
        "let mut inst = Instance::new();\n    \
         inst.func2(0, 0x5A);\n    \
         assert_eq!(inst.func0(0), 1);\n    \
         assert_eq!(inst.func0(1), 1);\n    \
         assert_eq!(inst.func1(), 2);\n    \
         assert_eq!(inst.func3(0), 0x5A);\n    \
         assert_eq!(inst.func0(0), 2);\n    \
         assert_eq!(inst.func0(1), -1);\n    \
         assert_eq!(inst.func1(), 2);",
    );
}

#[test]
fn memory_grow_with_maximum_zero_or_equal_to_initial_always_fails() {
    // `(memory 0 0)` and `(memory 2 2)` cannot grow at all; growing by zero is
    // still fine.
    transpile_compile_run(
        "mem_grow_max_zero",
        &grow_size_wat("0 0"),
        "let mut inst = Instance::new();\n    \
         assert_eq!(inst.func0(1), -1);\n    \
         assert_eq!(inst.func1(), 0);\n    \
         assert_eq!(inst.func0(0), 0);",
    );
    transpile_compile_run(
        "mem_grow_initial_is_max",
        &grow_size_wat("2 2"),
        "let mut inst = Instance::new();\n    \
         assert_eq!(inst.func0(1), -1);\n    \
         assert_eq!(inst.func1(), 2);\n    \
         assert_eq!(inst.func0(0), 2);",
    );
}

#[test]
fn memory_grow_without_declared_maximum_only_hits_the_wasm32_cap() {
    // No declared maximum: small growth succeeds, and growth past the 65536-page
    // wasm32 cap fails (checked before any allocation, so nothing big is made).
    transpile_compile_run(
        "mem_grow_no_max",
        &grow_size_wat("1"),
        "let mut inst = Instance::new();\n    \
         assert_eq!(inst.func0(3), 1);\n    \
         assert_eq!(inst.func1(), 4);\n    \
         assert_eq!(inst.func0(65536), -1);\n    \
         assert_eq!(inst.func1(), 4);",
    );
}

#[test]
fn store_at_offset_is_readable() {
    // A memarg offset is folded into the effective address.
    transpile_compile_run(
        "mem_offset",
        r#"
        (module
          (memory 1)
          (func (param i32) (i32.store offset=16 (local.get 0) (i32.const 777)))
          (func (param i32) (result i32) (i32.load offset=16 (local.get 0))))
        "#,
        "let mut inst = Instance::new();\n    \
         inst.func0(8);\n    \
         assert_eq!(inst.func1(8), 777);",
    );
}
