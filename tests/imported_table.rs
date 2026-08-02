//! Integration tests for imported tables. The host owns the `Vec<u32>` storage
//! and lends it through the injected `Imports` trait (`table`/`table_mut`); the
//! table entries are the module's own function indices (from element segments
//! or `ref.func`), so `call_indirect` still dispatches through a `match`, and
//! `table.get/set/size/grow` all act on the host buffer. Each module becomes a
//! `struct Instance<H: Imports>`, compiled with `rustc -D warnings` together
//! with a `Host` implementation, then exercised. Traps are a non-zero exit.

use std::process::Command;

/// Compile the transpiled module plus a trailing `extra` block (a `Host`
/// implementing `Imports`, and `fn main`), returning the binary path.
fn compile(test: &str, wat: &str, extra: &str) -> std::path::PathBuf {
    let wasm = wat::parse_str(wat).expect("valid wat");
    let generated = wasm2rs::transpile(&wasm).expect("transpile ok");

    let dir = std::env::temp_dir().join(format!("wasm2rs_imported_table_{test}"));
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

// A host that owns the funcref table storage as a `Vec<u32>`.
const HOST_TABLE: &str = r#"
    struct Host { table: Vec<u32> }
    impl Imports for Host {
        fn table(&self) -> &[u32] { &self.table }
        fn table_mut(&mut self) -> &mut Vec<u32> { &mut self.table }
    }
"#;

const BINOP_TABLE: &str = r#"
    (module
      (type $binop (func (param i32 i32) (result i32)))
      (import "env" "tbl" (table 2 funcref))
      (elem (i32.const 0) $add $sub)
      (func $add (param i32 i32) (result i32) (i32.add (local.get 0) (local.get 1)))
      (func $sub (param i32 i32) (result i32) (i32.sub (local.get 0) (local.get 1)))
      (func $dispatch (param i32 i32 i32) (result i32)
        (call_indirect (type $binop) (local.get 1) (local.get 2) (local.get 0))))
    "#;

#[test]
fn imported_table_dispatch_with_active_element() {
    // The active element segment writes the module's function indices into the
    // host-owned table at instantiation; call_indirect dispatches through it.
    let extra = format!(
        "{HOST_TABLE}\n\
         fn main() {{\n    \
         let host = Host {{ table: vec![u32::MAX; 2] }};\n    \
         let mut inst = Instance::new(host);\n    \
         assert_eq!(inst.func2(0, 10, 3), 13);\n    \
         assert_eq!(inst.func2(1, 10, 3), 7);\n    \
         }}"
    );
    let bin = compile("dispatch", BINOP_TABLE, &extra);
    let run = Command::new(&bin).status().expect("run generated binary");
    assert!(run.success(), "imported table dispatch failed");
}

#[test]
fn imported_table_call_indirect_out_of_bounds_traps() {
    let extra = format!(
        "{HOST_TABLE}\n\
         fn main() {{\n    \
         let host = Host {{ table: vec![u32::MAX; 2] }};\n    \
         let mut inst = Instance::new(host);\n    \
         inst.func2(5, 10, 3);\n    \
         }}"
    );
    let bin = compile("oob", BINOP_TABLE, &extra);
    let run = Command::new(&bin).output().expect("run generated binary");
    assert!(
        !run.status.success(),
        "expected a trap indexing past the host table"
    );
}

#[test]
fn imported_table_active_element_out_of_bounds_traps_at_instantiation() {
    // The active element writes slots 0 and 1, but the host lends a single-slot
    // buffer, so instantiation (`Instance::new`) traps while applying it.
    let extra = format!(
        "{HOST_TABLE}\n\
         fn main() {{\n    \
         let host = Host {{ table: vec![u32::MAX; 1] }};\n    \
         let _inst = Instance::new(host);\n    \
         }}"
    );
    let bin = compile("elem_oob", BINOP_TABLE, &extra);
    let run = Command::new(&bin).output().expect("run generated binary");
    assert!(
        !run.status.success(),
        "expected an instantiation trap applying the active element"
    );
}

#[test]
fn imported_table_get_set_reroute_dispatch() {
    // `table.get`/`table.set` read and rewrite host-table entries: after copying
    // slot 1's funcref into slot 0, an indirect call through slot 0 reaches $b.
    let wat = r#"
        (module
          (type $f (func (result i32)))
          (import "env" "tbl" (table 2 funcref))
          (elem (i32.const 0) $a $b)
          (func $a (result i32) (i32.const 111))
          (func $b (result i32) (i32.const 222))
          (func $swap (table.set (i32.const 0) (table.get (i32.const 1))))
          (func $call0 (result i32) (call_indirect (type $f) (i32.const 0))))
        "#;
    let extra = format!(
        "{HOST_TABLE}\n\
         fn main() {{\n    \
         let host = Host {{ table: vec![u32::MAX; 2] }};\n    \
         let mut inst = Instance::new(host);\n    \
         assert_eq!(inst.func3(), 111);\n    \
         inst.func2();\n    \
         assert_eq!(inst.func3(), 222);\n    \
         }}"
    );
    let bin = compile("get_set", wat, &extra);
    let run = Command::new(&bin).status().expect("run generated binary");
    assert!(run.success(), "imported table get/set reroute failed");
}

#[test]
fn imported_table_size_and_grow_track_host_buffer() {
    // `table.size`/`table.grow` observe and extend the host-owned buffer.
    let wat = r#"
        (module
          (import "env" "tbl" (table 1 funcref))
          (func (result i32) (table.size))
          (func (param i32) (result i32) (table.grow (ref.null func) (local.get 0))))
        "#;
    let extra = format!(
        "{HOST_TABLE}\n\
         fn main() {{\n    \
         let host = Host {{ table: vec![u32::MAX; 1] }};\n    \
         let mut inst = Instance::new(host);\n    \
         assert_eq!(inst.func0(), 1);\n    \
         assert_eq!(inst.func1(3), 1);\n    \
         assert_eq!(inst.func0(), 4);\n    \
         }}"
    );
    let bin = compile("size_grow", wat, &extra);
    let run = Command::new(&bin).status().expect("run generated binary");
    assert!(run.success(), "imported table size/grow failed");
}
