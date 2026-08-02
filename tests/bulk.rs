//! Integration tests for Phase 7d: bulk-memory instructions — `memory.fill`,
//! `memory.copy` and `table.copy`. Each module carries state (memory or a
//! table), so it becomes a `struct Instance`; the generated Rust is compiled
//! with `rustc -D warnings` and exercised. Traps are verified by the generated
//! binary exiting unsuccessfully.

use std::process::Command;

/// Transpile `wat`, wrap the output in a `main` running `main_body`, and compile
/// it with `rustc -D warnings`. Returns the path to the built binary.
fn compile(test: &str, wat: &str, main_body: &str) -> std::path::PathBuf {
    let wasm = wat::parse_str(wat).expect("valid wat");
    let generated = wasm2rs::transpile(&wasm).expect("transpile ok");

    let dir = std::env::temp_dir().join(format!("wasm2rs_bulk_{test}"));
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

// A module exercising `memory.fill`/`memory.copy` alongside byte store/load so a
// test can set up and observe memory contents.
const MEM_M: &str = r#"
    (module
      (memory 1)
      (func (param i32 i32 i32) (memory.fill (local.get 0) (local.get 1) (local.get 2)))
      (func (param i32 i32 i32) (memory.copy (local.get 0) (local.get 1) (local.get 2)))
      (func (param i32 i32) (i32.store8 (local.get 0) (local.get 1)))
      (func (param i32) (result i32) (i32.load8_u (local.get 0))))
    "#;

#[test]
fn memory_fill_sets_a_range() {
    transpile_compile_run(
        "mem_fill",
        MEM_M,
        "let mut inst = Instance::new();\n    \
         inst.func0(4, 0xAB, 3);\n    \
         assert_eq!(inst.func3(4), 0xAB);\n    \
         assert_eq!(inst.func3(6), 0xAB);\n    \
         assert_eq!(inst.func3(7), 0);\n    \
         assert_eq!(inst.func3(3), 0);",
    );
}

#[test]
fn memory_copy_handles_overlap() {
    // Bytes 1,2,3 at [0..3] copied forward to [1..4] must land as 1,2,3 (memmove
    // semantics), leaving [0] untouched.
    transpile_compile_run(
        "mem_copy",
        MEM_M,
        "let mut inst = Instance::new();\n    \
         inst.func2(0, 1);\n    \
         inst.func2(1, 2);\n    \
         inst.func2(2, 3);\n    \
         inst.func1(1, 0, 3);\n    \
         assert_eq!(inst.func3(0), 1);\n    \
         assert_eq!(inst.func3(1), 1);\n    \
         assert_eq!(inst.func3(2), 2);\n    \
         assert_eq!(inst.func3(3), 3);",
    );
}

#[test]
fn memory_fill_out_of_bounds_traps() {
    let bin = compile(
        "mem_fill_oob",
        MEM_M,
        "let mut inst = Instance::new(); inst.func0(65535, 0, 10);",
    );
    let run = Command::new(&bin).output().expect("run generated binary");
    assert!(
        !run.status.success(),
        "expected a trap on out-of-bounds fill"
    );
}

#[test]
fn memory_fill_zero_len_at_end_does_not_trap() {
    // wasm allows a zero-length fill/copy whose offset equals the memory size
    // (size 65536 for one page); it must not trap.
    transpile_compile_run(
        "mem_fill_zero_len",
        MEM_M,
        "let mut inst = Instance::new();\n    \
         inst.func0(65536, 0, 0);\n    \
         inst.func1(65536, 65536, 0);",
    );
}

#[test]
fn memory_copy_out_of_bounds_traps() {
    let bin = compile(
        "mem_copy_oob",
        MEM_M,
        "let mut inst = Instance::new(); inst.func1(65530, 0, 10);",
    );
    let run = Command::new(&bin).output().expect("run generated binary");
    assert!(
        !run.status.success(),
        "expected a trap on out-of-bounds copy"
    );
}

// A table of four funcref slots, the first two initialised to functions
// returning 100 and 200, plus `table.copy` and an indirect caller to observe it.
const TABLE_M: &str = r#"
    (module
      (type $sig (func (result i32)))
      (table 4 funcref)
      (elem (i32.const 0) $a $b)
      (func $a (type $sig) (i32.const 100))
      (func $b (type $sig) (i32.const 200))
      (func (param i32 i32 i32) (table.copy (local.get 0) (local.get 1) (local.get 2)))
      (func (param i32) (result i32) (call_indirect (type $sig) (local.get 0))))
    "#;

#[test]
fn table_copy_moves_entries() {
    // Slot 1 (-> $b) is copied into slot 2, so an indirect call through slot 2
    // then dispatches to $b.
    transpile_compile_run(
        "table_copy",
        TABLE_M,
        "let mut inst = Instance::new();\n    \
         assert_eq!(inst.func3(0), 100);\n    \
         assert_eq!(inst.func3(1), 200);\n    \
         inst.func2(2, 1, 1);\n    \
         assert_eq!(inst.func3(2), 200);",
    );
}

#[test]
fn table_copy_out_of_bounds_traps() {
    let bin = compile(
        "table_copy_oob",
        TABLE_M,
        "let mut inst = Instance::new(); inst.func2(2, 0, 10);",
    );
    let run = Command::new(&bin).output().expect("run generated binary");
    assert!(
        !run.status.success(),
        "expected a trap on out-of-bounds table copy"
    );
}
