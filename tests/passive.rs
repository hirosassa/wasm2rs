//! Integration tests for Phase 6g: passive data/element segments and the
//! bulk-init instructions `memory.init`/`data.drop`/`table.init`/`elem.drop`. A
//! passive segment is retained (not auto-copied at instantiation) and copied
//! into memory/table on demand; dropping it makes further inits of non-zero
//! length trap. Each module becomes a `struct Instance`; the generated Rust is
//! compiled with `rustc -D warnings` and exercised. Traps are verified by the
//! generated binary exiting unsuccessfully.

use std::process::Command;

fn compile(test: &str, wat: &str, main_body: &str) -> std::path::PathBuf {
    let wasm = wat::parse_str(wat).expect("valid wat");
    let generated = wasm2rs::transpile(&wasm).expect("transpile ok");

    let dir = std::env::temp_dir().join(format!("wasm2rs_passive_{test}"));
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

fn transpile_compile_run(test: &str, wat: &str, main_body: &str) {
    let bin = compile(test, wat, main_body);
    let run = Command::new(&bin).status().expect("run generated binary");
    assert!(
        run.success(),
        "generated program assertions failed:\n{test}"
    );
}

// A passive data segment plus `memory.init`, `data.drop` and a byte load.
const DATA_M: &str = r#"
    (module
      (memory 1)
      (data "\01\02\03\04")
      (func (param i32 i32 i32) (memory.init 0 (local.get 0) (local.get 1) (local.get 2)))
      (func (param i32) (result i32) (i32.load8_u (local.get 0)))
      (func (data.drop 0)))
    "#;

#[test]
fn memory_init_copies_from_passive_data() {
    transpile_compile_run(
        "mem_init",
        DATA_M,
        "let mut inst = Instance::new();\n    \
         inst.func0(10, 1, 2);\n    \
         assert_eq!(inst.func1(10), 2);\n    \
         assert_eq!(inst.func1(11), 3);\n    \
         assert_eq!(inst.func1(12), 0);",
    );
}

#[test]
fn memory_init_out_of_bounds_source_traps() {
    // src=2, len=4 reads past the 4-byte segment.
    let bin = compile(
        "mem_init_oob",
        DATA_M,
        "let mut inst = Instance::new(); inst.func0(0, 2, 4);",
    );
    let run = Command::new(&bin).output().expect("run generated binary");
    assert!(
        !run.status.success(),
        "expected a trap reading past the segment"
    );
}

#[test]
fn memory_init_out_of_bounds_dest_traps() {
    // dest=65534, len=3 writes past the 65536-byte page.
    let bin = compile(
        "mem_init_dest_oob",
        DATA_M,
        "let mut inst = Instance::new(); inst.func0(65534, 0, 3);",
    );
    let run = Command::new(&bin).output().expect("run generated binary");
    assert!(!run.status.success(), "expected a trap writing past memory");
}

#[test]
fn memory_init_zero_len_at_end_does_not_trap() {
    // A zero-length init whose dest equals the memory size must not trap.
    transpile_compile_run(
        "mem_init_zero_len",
        DATA_M,
        "let mut inst = Instance::new();\n    \
         inst.func0(65536, 0, 0);",
    );
}

#[test]
fn memory_init_after_data_drop_traps() {
    // After dropping, a non-zero-length init sees an empty segment and traps.
    let bin = compile(
        "mem_init_dropped",
        DATA_M,
        "let mut inst = Instance::new(); inst.func2(); inst.func0(0, 0, 1);",
    );
    let run = Command::new(&bin).output().expect("run generated binary");
    assert!(!run.status.success(), "expected a trap after data.drop");
}

// A passive element segment plus `table.init`, `elem.drop` and an indirect call.
const ELEM_M: &str = r#"
    (module
      (type $sig (func (result i32)))
      (table 4 funcref)
      (func $a (type $sig) (i32.const 11))
      (func $b (type $sig) (i32.const 22))
      (elem func $a $b)
      (func (param i32 i32 i32) (table.init 0 (local.get 0) (local.get 1) (local.get 2)))
      (func (param i32) (result i32) (call_indirect (type $sig) (local.get 0)))
      (func (elem.drop 0)))
    "#;

#[test]
fn table_init_copies_from_passive_element() {
    // Copy both entries into slots 0,1, then dispatch through them.
    transpile_compile_run(
        "table_init",
        ELEM_M,
        "let mut inst = Instance::new();\n    \
         inst.func2(0, 0, 2);\n    \
         assert_eq!(inst.func3(0), 11);\n    \
         assert_eq!(inst.func3(1), 22);",
    );
}

#[test]
fn table_init_out_of_bounds_source_traps() {
    // src=0, len=3 reads past the 2-entry element segment.
    let bin = compile(
        "table_init_src_oob",
        ELEM_M,
        "let mut inst = Instance::new(); inst.func2(0, 0, 3);",
    );
    let run = Command::new(&bin).output().expect("run generated binary");
    assert!(
        !run.status.success(),
        "expected a trap reading past the element segment"
    );
}

#[test]
fn table_init_after_elem_drop_traps() {
    let bin = compile(
        "table_init_dropped",
        ELEM_M,
        "let mut inst = Instance::new(); inst.func4(); inst.func2(0, 0, 1);",
    );
    let run = Command::new(&bin).output().expect("run generated binary");
    assert!(!run.status.success(), "expected a trap after elem.drop");
}

#[test]
fn memory_init_on_active_segment_is_unsupported() {
    // `memory.init` only references passive segments here; an active segment is
    // auto-copied and implicitly dropped, so referencing it is rejected.
    let wat = r#"
        (module
          (memory 1)
          (data (i32.const 0) "\01\02")
          (func (memory.init 0 (i32.const 0) (i32.const 0) (i32.const 0))))
        "#;
    let wasm = wat::parse_str(wat).expect("valid wat");
    assert!(
        wasm2rs::transpile(&wasm).is_err(),
        "memory.init on an active segment must be rejected"
    );
}
