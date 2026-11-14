//! Integration tests for imported linear memory (part of Phase 5). The host
//! owns the `Vec<u8>` and lends it through the injected `Imports` trait
//! (`memory`/`memory_mut`); the module's loads, stores and `memory.grow` all
//! act on that host buffer. Each module becomes a `struct Instance<H: Imports>`;
//! the generated Rust is compiled with `rustc -D warnings` (together with a Host
//! implementation) and exercised. Traps are verified by a non-zero exit.

use std::process::Command;

/// [`compile_with`] using the default options.
fn compile(test: &str, wat: &str, extra: &str) -> std::path::PathBuf {
    compile_with(test, wat, extra, &wasm2rs::TranspileOptions::default())
}

/// Compile the transpiled module plus a trailing `extra` block (a `Host`
/// implementing `Imports`, and `fn main`).
fn compile_with(
    test: &str,
    wat: &str,
    extra: &str,
    opts: &wasm2rs::TranspileOptions,
) -> std::path::PathBuf {
    let wasm = wat::parse_str(wat).expect("valid wat");
    let generated = wasm2rs::transpile_with_options(&wasm, opts).expect("transpile ok");

    let dir = crate::common::scratch_dir(&format!("wasm2rs_imported_memory_{test}"));
    let src = dir.join("gen.rs");
    let bin = dir.join(if cfg!(windows) { "gen.exe" } else { "gen" });

    let program = format!("{generated}\n{extra}\n");
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

#[test]
fn imported_memory_grow_respects_module_declared_maximum() {
    // The host `Vec` has no wasm maximum of its own; the module's declared
    // maximum of two pages still bounds `memory.grow`, which fails before
    // touching the host buffer.
    let wat = r#"
        (module
          (import "env" "mem" (memory 1 2))
          (func (param i32) (result i32) (memory.grow (local.get 0)))
          (func (result i32) (memory.size))
          (func (param i32 i32) (i32.store8 (local.get 0) (local.get 1)))
          (func (param i32) (result i32) (i32.load8_u (local.get 0))))
        "#;
    let extra = format!(
        "{HOST_1_PAGE}\n\
         fn main() {{\n    \
         let mut inst = Instance::new(Host {{ mem: vec![0u8; 65536] }});\n    \
         inst.func2(0, 0x5A);\n    \
         assert_eq!(inst.func0(2), -1);\n    \
         assert_eq!(inst.func1(), 1);\n    \
         assert_eq!(inst.imports.mem.len(), 65536);\n    \
         assert_eq!(inst.func0(1), 1);\n    \
         assert_eq!(inst.func1(), 2);\n    \
         assert_eq!(inst.func0(0), 2);\n    \
         assert_eq!(inst.func0(1), -1);\n    \
         assert_eq!(inst.func1(), 2);\n    \
         assert_eq!(inst.imports.mem.len(), 2 * 65536);\n    \
         assert_eq!(inst.func3(0), 0x5A);\n    \
         }}"
    );
    let bin = compile("grow_declared_maximum", wat, &extra);
    let run = Command::new(&bin).status().expect("run generated binary");
    assert!(run.success(), "imported memory maximum was not enforced");
}

#[test]
fn import_memory_option_keeps_the_defined_memorys_maximum() {
    // With `import_memory` a defined `(memory 1 2)` becomes host-owned but its
    // declared maximum still applies.
    let wat = r#"
        (module
          (memory 1 2)
          (func (param i32) (result i32) (memory.grow (local.get 0)))
          (func (result i32) (memory.size)))
        "#;
    let extra = format!(
        "{HOST_1_PAGE}\n\
         fn main() {{\n    \
         let mut inst = Instance::new(Host {{ mem: vec![0u8; 65536] }});\n    \
         assert_eq!(inst.func0(2), -1);\n    \
         assert_eq!(inst.func0(1), 1);\n    \
         assert_eq!(inst.func0(1), -1);\n    \
         assert_eq!(inst.func1(), 2);\n    \
         }}"
    );
    let bin = compile_with("import_memory_max", wat, &extra, &import_memory());
    let run = Command::new(&bin).status().expect("run generated binary");
    assert!(
        run.success(),
        "defined-then-imported memory maximum ignored"
    );
}

fn import_memory() -> wasm2rs::TranspileOptions {
    wasm2rs::TranspileOptions::default().with_import_memory(true)
}

#[test]
fn import_memory_makes_a_defined_memory_host_owned() {
    // The module defines its memory and a data segment; with `import_memory` the
    // host lends the buffer, the segment lands in it, loads and stores act on it
    // and the host sees the writes.
    let wat = r#"
        (module
          (memory 1)
          (data (i32.const 8) "\01\02\03\04")
          (func (param i32) (result i32) (i32.load8_u (local.get 0)))
          (func (param i32 i32) (i32.store8 (local.get 0) (local.get 1))))
        "#;
    let extra = format!(
        "{HOST_1_PAGE}\n\
         fn main() {{\n    \
         let mut inst = Instance::new(Host {{ mem: vec![0u8; 65536] }});\n    \
         assert_eq!(inst.func0(8), 1);\n    \
         assert_eq!(inst.func0(11), 4);\n    \
         inst.func1(100, 42);\n    \
         assert_eq!(inst.func0(100), 42);\n    \
         assert_eq!(inst.imports.mem[100], 42);\n    \
         }}"
    );
    let bin = compile_with("import_memory_defined", wat, &extra, &import_memory());
    let run = Command::new(&bin).status().expect("run generated binary");
    assert!(run.success(), "a host-owned defined memory failed");
}

#[test]
fn import_memory_grows_the_host_buffer() {
    let wat = r#"
        (module
          (memory 1)
          (func (result i32) (memory.grow (i32.const 1)))
          (func (result i32) (memory.size)))
        "#;
    let extra = format!(
        "{HOST_1_PAGE}\n\
         fn main() {{\n    \
         let mut inst = Instance::new(Host {{ mem: vec![0u8; 65536] }});\n    \
         assert_eq!(inst.func0(), 1);\n    \
         assert_eq!(inst.func1(), 2);\n    \
         assert_eq!(inst.imports.mem.len(), 2 * 65536);\n    \
         }}"
    );
    let bin = compile_with("import_memory_grow", wat, &extra, &import_memory());
    let run = Command::new(&bin).status().expect("run generated binary");
    assert!(run.success(), "growing a host-owned defined memory failed");
}

#[test]
fn import_memory_leaves_a_module_without_memory_alone() {
    let wat = "(module (func (result i32) (i32.const 7)))";
    let extra = "fn main() { assert_eq!(func0(), 7); }";
    let bin = compile_with("import_memory_none", wat, extra, &import_memory());
    let run = Command::new(&bin).status().expect("run generated binary");
    assert!(run.success(), "a module without memory changed shape");
}

#[test]
fn import_memory_leaves_an_imported_memory_alone() {
    let extra = format!(
        "{HOST_1_PAGE}\n\
         fn main() {{\n    \
         let mut host = Host {{ mem: vec![0u8; 65536] }};\n    \
         host.mem[20] = 7;\n    \
         let mut inst = Instance::new(host);\n    \
         assert_eq!(inst.func0(20), 7);\n    \
         }}"
    );
    let bin = compile_with(
        "import_memory_already",
        LOAD_STORE_M,
        &extra,
        &import_memory(),
    );
    let run = Command::new(&bin).status().expect("run generated binary");
    assert!(run.success(), "an imported memory changed shape");
}

#[test]
fn import_memory_leaves_a_shared_memory_alone() {
    // A shared memory keeps its thread-shareable backing: no `memory` accessor
    // is added to the host trait, and `new` still needs no host.
    let wat = r#"
        (module
          (memory 1 1 shared)
          (func (result i32) (i32.const 7)))
        "#;
    let extra = "fn main() {\n    \
         let mut inst = Instance::new();\n    \
         assert_eq!(inst.func0(), 7);\n    \
         }";
    let bin = compile_with("import_memory_shared", wat, extra, &import_memory());
    let run = Command::new(&bin).status().expect("run generated binary");
    assert!(run.success(), "a shared memory changed shape");
}

// A host whose storage outlives the instance (raw pointers), so a test can look at
// it after `Instance::new` traps.
const MEM_HOST: &str = r#"
    struct Host { mem: *mut Vec<u8> }
    impl Imports for Host {
        fn memory(&self) -> &[u8] { unsafe { &*self.mem } }
        fn memory_mut(&mut self) -> &mut Vec<u8> { unsafe { &mut *self.mem } }
    }
    fn leak<T>(value: T) -> *mut T { Box::leak(Box::new(value)) }
"#;

const MEM_AND_TABLE_HOST: &str = r#"
    struct Host { mem: *mut Vec<u8>, table: *mut Vec<u32> }
    impl Imports for Host {
        fn memory(&self) -> &[u8] { unsafe { &*self.mem } }
        fn memory_mut(&mut self) -> &mut Vec<u8> { unsafe { &mut *self.mem } }
        fn table(&self) -> &[u32] { unsafe { &*self.table } }
        fn table_mut(&mut self) -> &mut Vec<u32> { unsafe { &mut *self.table } }
    }
    fn leak<T>(value: T) -> *mut T { Box::leak(Box::new(value)) }
"#;

#[test]
fn data_segments_apply_in_module_order_across_an_imported_and_a_defined_memory() {
    // Segment 0 writes the host memory, segment 1 (memory 1) is out of bounds and
    // traps: the spec applies segments in module order, so the host keeps segment
    // 0's byte.
    let wat = r#"
        (module
          (import "env" "mem" (memory $m0 1))
          (memory $m1 1)
          (data (memory $m0) (i32.const 0) "a")
          (data (memory $m1) (i32.const 65536) "b"))
        "#;
    let extra = format!(
        "{MEM_HOST}\n\
         fn main() {{\n    \
         let mem = leak(vec![0u8; 65536]);\n    \
         std::panic::set_hook(Box::new(|_| {{}}));\n    \
         let host = Host {{ mem }};\n    \
         let r = std::panic::catch_unwind(move || {{ let _ = Instance::new(host); }});\n    \
         assert!(r.is_err(), \"the out-of-bounds segment must trap\");\n    \
         assert_eq!(unsafe {{ &*mem }}[0], b'a', \"segment 0 ran before the trap\");\n    \
         }}"
    );
    let bin = compile("data_order", wat, &extra);
    let run = Command::new(&bin).status().expect("run generated binary");
    assert!(
        run.success(),
        "data segments were not applied in module order"
    );
}

#[test]
fn elements_come_before_a_trapping_data_segment_of_a_defined_memory() {
    // All active elements are applied before any data segment, so the host table
    // already holds the element when the defined memory's segment traps.
    let wat = r#"
        (module
          (import "env" "mem" (memory $m0 1))
          (import "env" "tbl" (table $t0 1 funcref))
          (memory $m1 1)
          (func $f)
          (elem (table $t0) (i32.const 0) func $f)
          (data (memory $m1) (i32.const 65536) "b"))
        "#;
    let extra = format!(
        "{MEM_AND_TABLE_HOST}\n\
         fn main() {{\n    \
         let table = leak(vec![u32::MAX; 1]);\n    \
         std::panic::set_hook(Box::new(|_| {{}}));\n    \
         let host = Host {{ mem: leak(vec![0u8; 65536]), table }};\n    \
         let r = std::panic::catch_unwind(move || {{ let _ = Instance::new(host); }});\n    \
         assert!(r.is_err(), \"the out-of-bounds segment must trap\");\n    \
         assert_eq!(unsafe {{ &*table }}[0], 0, \"the element ran before the data trap\");\n    \
         }}"
    );
    let bin = compile("elem_before_data", wat, &extra);
    let run = Command::new(&bin).status().expect("run generated binary");
    assert!(run.success(), "an element did not precede the data trap");
}
