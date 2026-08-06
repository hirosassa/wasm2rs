//! Integration tests for imported linear memory (part of Phase 5). The host
//! owns the `Vec<u8>` and lends it through the injected `Imports` trait
//! (`memory`/`memory_mut`); the module's loads, stores and `memory.grow` all
//! act on that host buffer. Each module becomes a `struct Instance<H: Imports>`;
//! the generated Rust is compiled with `rustc -D warnings` (together with a Host
//! implementation) and exercised. Traps are verified by a non-zero exit.

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

/// Compile the transpiled module plus a trailing `extra` block (a `Host`
/// implementing `Imports`, and `fn main`).
fn compile(test: &str, wat: &str, extra: &str) -> std::path::PathBuf {
    let wasm = wat::parse_str(wat).expect("valid wat");
    let generated = wasm2rs::transpile(&wasm).expect("transpile ok");

    let dir = std::env::temp_dir().join(format!("wasm2rs_imported_memory_{test}"));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let src = dir.join("gen.rs");
    let bin = dir.join(if cfg!(windows) { "gen.exe" } else { "gen" });

    let program = format!("{generated}\n{extra}\n");
    std::fs::write(&src, &program).expect("write generated source");

    let out = Command::new("rustc")
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

// A host-owned 1-page memory, read and written by the module.
const LOAD_STORE_M: &str = r#"
    (module
      (import "env" "mem" (memory 1))
      (func (param i32) (result i32) (i32.load8_u (local.get 0)))
      (func (param i32 i32) (i32.store8 (local.get 0) (local.get 1))))
    "#;

const HOST_1_PAGE: &str = r#"
    struct Host { mem: Vec<u8> }
    impl Imports for Host {
        fn memory(&self) -> &[u8] { &self.mem }
        fn memory_mut(&mut self) -> &mut Vec<u8> { &mut self.mem }
    }
"#;

#[test]
fn imported_memory_reads_host_bytes_and_writes_persist() {
    let extra = format!(
        "{HOST_1_PAGE}\n\
         fn main() {{\n    \
         let mut host = Host {{ mem: vec![0u8; 65536] }};\n    \
         host.mem[20] = 7;\n    \
         let mut inst = Instance::new(host);\n    \
         assert_eq!(inst.func0(20), 7);\n    \
         inst.func1(21, 99);\n    \
         assert_eq!(inst.func0(21), 99);\n    \
         }}"
    );
    let bin = compile("load_store", LOAD_STORE_M, &extra);
    let run = Command::new(&bin).status().expect("run generated binary");
    assert!(run.success(), "imported memory round-trip failed");
}

#[test]
fn imported_memory_load_out_of_bounds_traps() {
    let extra = format!(
        "{HOST_1_PAGE}\n\
         fn main() {{\n    \
         let host = Host {{ mem: vec![0u8; 65536] }};\n    \
         let mut inst = Instance::new(host);\n    \
         inst.func0(65536);\n    \
         }}"
    );
    let bin = compile("load_oob", LOAD_STORE_M, &extra);
    let run = Command::new(&bin).output().expect("run generated binary");
    assert!(
        !run.status.success(),
        "expected a trap reading past the host memory"
    );
}

#[test]
fn imported_memory_active_data_initialises_host_buffer() {
    // An active data segment on imported memory is written into the host buffer
    // at instantiation (`Instance::new`), so a later load reads those bytes.
    let wat = r#"
        (module
          (import "env" "mem" (memory 1))
          (data (i32.const 8) "\01\02\03\04")
          (func (param i32) (result i32) (i32.load8_u (local.get 0))))
        "#;
    let extra = format!(
        "{HOST_1_PAGE}\n\
         fn main() {{\n    \
         let host = Host {{ mem: vec![0u8; 65536] }};\n    \
         let mut inst = Instance::new(host);\n    \
         assert_eq!(inst.func0(8), 1);\n    \
         assert_eq!(inst.func0(11), 4);\n    \
         assert_eq!(inst.func0(7), 0);\n    \
         assert_eq!(inst.func0(12), 0);\n    \
         }}"
    );
    let bin = compile("active_data", wat, &extra);
    let run = Command::new(&bin).status().expect("run generated binary");
    assert!(run.success(), "imported memory active data init failed");
}

#[test]
fn imported_memory_multiple_active_data_segments_apply_in_order() {
    // Two overlapping active data segments must be applied in declaration order:
    // the second segment (offset 10) overwrites the tail of the first (offset 8).
    let wat = r#"
        (module
          (import "env" "mem" (memory 1))
          (data (i32.const 8) "\01\02\03\04")
          (data (i32.const 10) "\09\09")
          (func (param i32) (result i32) (i32.load8_u (local.get 0))))
        "#;
    let extra = format!(
        "{HOST_1_PAGE}\n\
         fn main() {{\n    \
         let host = Host {{ mem: vec![0u8; 65536] }};\n    \
         let mut inst = Instance::new(host);\n    \
         assert_eq!(inst.func0(8), 1);\n    \
         assert_eq!(inst.func0(9), 2);\n    \
         assert_eq!(inst.func0(10), 9);\n    \
         assert_eq!(inst.func0(11), 9);\n    \
         }}"
    );
    let bin = compile("active_data_order", wat, &extra);
    let run = Command::new(&bin).status().expect("run generated binary");
    assert!(run.success(), "imported memory active data order failed");
}

#[test]
fn imported_memory_grow_extends_the_host_buffer() {
    // `memory.grow` on imported memory grows the host `Vec`; a store into the
    // new page then round-trips.
    let wat = r#"
        (module
          (import "env" "mem" (memory 1))
          (func (result i32) (memory.grow (i32.const 1)))
          (func (param i32 i32) (i32.store8 (local.get 0) (local.get 1)))
          (func (param i32) (result i32) (i32.load8_u (local.get 0))))
        "#;
    let extra = format!(
        "{HOST_1_PAGE}\n\
         fn main() {{\n    \
         let host = Host {{ mem: vec![0u8; 65536] }};\n    \
         let mut inst = Instance::new(host);\n    \
         assert_eq!(inst.func0(), 1);\n    \
         inst.func1(70000, 5);\n    \
         assert_eq!(inst.func2(70000), 5);\n    \
         }}"
    );
    let bin = compile("grow", wat, &extra);
    let run = Command::new(&bin).status().expect("run generated binary");
    assert!(run.success(), "imported memory grow failed");
}
