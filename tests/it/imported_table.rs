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
fn an_active_element_item_of_an_imported_table_may_read_an_imported_global() {
    // The host-owned table is written after construction, so the item reads the
    // host through the built instance.
    let wat = r#"
        (module
          (type $t (func (result i32)))
          (import "env" "f" (global funcref))
          (import "env" "tbl" (table 2 funcref))
          (elem (i32.const 1) funcref (global.get 0))
          (func (type $t) (i32.const 7))
          (func (param i32) (result i32) (call_indirect (type $t) (local.get 0))))
        "#;
    let extra = "struct Host { table: Vec<u32> }\n\
         impl Imports for Host {\n    \
             fn get_global0(&self) -> u32 { 0 }\n    \
             fn table(&self) -> &[u32] { &self.table }\n    \
             fn table_mut(&mut self) -> &mut Vec<u32> { &mut self.table }\n\
         }\n\
         fn main() {\n    \
         let mut inst = Instance::new(Host { table: vec![u32::MAX; 2] });\n    \
         assert_eq!(inst.func1(1), 7);\n    \
         }";
    let bin = compile("item_imported_global", wat, extra);
    let run = Command::new(&bin).status().expect("run generated binary");
    assert!(
        run.success(),
        "imported-global item in an imported table failed"
    );
}

#[test]
fn a_shared_memory_module_applies_active_elements_to_an_imported_table() {
    // With a shared memory the constructors take a different path; both `new` and
    // `with_memory` (a sibling instance with its own host table) must still copy
    // the active element segment into the host-owned table.
    let wat = r#"
        (module
          (type $t (func (result i32)))
          (import "env" "tbl" (table 2 funcref))
          (memory 1 1 shared)
          (elem (i32.const 1) $seven)
          (func $seven (type $t) (i32.const 7))
          (func (param i32) (result i32) (call_indirect (type $t) (local.get 0))))
        "#;
    let extra = format!(
        "{HOST_TABLE}\n\
         fn main() {{\n    \
         let mut a = Instance::new(Host {{ table: vec![u32::MAX; 2] }});\n    \
         assert_eq!(a.func1(1), 7);\n    \
         let mut b = Instance::with_memory(a.shared_memory(), Host {{ table: vec![u32::MAX; 2] }});\n    \
         assert_eq!(b.func1(1), 7);\n    \
         }}"
    );
    let bin = compile("shared_elems", wat, &extra);
    let run = Command::new(&bin).status().expect("run generated binary");
    assert!(
        run.success(),
        "shared-memory imported-table elements failed"
    );
}

#[test]
fn a_shared_memory_start_function_sees_the_imported_table_elements() {
    // The start function dispatches through slot 1: it traps unless the active
    // element was copied into the host table before it ran.
    let wat = r#"
        (module
          (type $t (func))
          (import "env" "tbl" (table 2 funcref))
          (memory 1 1 shared)
          (elem (i32.const 1) $nop)
          (func $nop (type $t))
          (func $boot (call_indirect (type $t) (i32.const 1)))
          (start $boot))
        "#;
    let extra = format!(
        "{HOST_TABLE}\n\
         fn main() {{\n    \
         let _a = Instance::new(Host {{ table: vec![u32::MAX; 2] }});\n    \
         }}"
    );
    let bin = compile("shared_start", wat, &extra);
    let run = Command::new(&bin).status().expect("run generated binary");
    assert!(run.success(), "the start function ran before the elements");
}

#[test]
fn a_trapping_element_leaves_the_imported_memory_untouched() {
    // Instantiation applies elements before data, so an element that does not fit
    // the host table traps before any data reaches the host memory.
    let wat = r#"
        (module
          (import "env" "mem" (memory 1))
          (import "env" "tbl" (table 1 funcref))
          (elem (i32.const 5) $f)
          (data (i32.const 0) "\01")
          (func $f))
        "#;
    let extra = r#"
        static DIRTY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        struct Host { mem: Vec<u8>, table: Vec<u32> }
        impl Drop for Host {
            fn drop(&mut self) {
                if self.mem[0] != 0 { DIRTY.store(true, std::sync::atomic::Ordering::SeqCst); }
            }
        }
        impl Imports for Host {
            fn memory(&self) -> &[u8] { &self.mem }
            fn memory_mut(&mut self) -> &mut Vec<u8> { &mut self.mem }
            fn table(&self) -> &[u32] { &self.table }
            fn table_mut(&mut self) -> &mut Vec<u32> { &mut self.table }
        }
        fn main() {
            let host = Host { mem: vec![0; 65536], table: vec![u32::MAX; 1] };
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| Instance::new(host)));
            assert!(r.is_err(), "the element should trap");
            assert!(!DIRTY.load(std::sync::atomic::Ordering::SeqCst), "data was written before the trap");
        }
    "#;
    let bin = compile("elem_before_data", wat, extra);
    let run = Command::new(&bin).output().expect("run generated binary");
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn a_trapping_element_leaves_an_imported_shared_memory_untouched() {
    // The same, through the shared-memory constructor.
    let wat = r#"
        (module
          (import "env" "mem" (memory 1 1 shared))
          (import "env" "tbl" (table 1 funcref))
          (elem (i32.const 5) $f)
          (data (i32.const 0) "\01")
          (func $f))
        "#;
    let extra = format!(
        "{HOST_TABLE}\n\
         fn main() {{\n    \
         let m = SharedMemory::new(65536);\n    \
         let host = Host {{ table: vec![u32::MAX; 1] }};\n    \
          let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| Instance::new(m.clone(), host)));\n    \
         assert!(r.is_err(), \"the element should trap\");\n    \
         assert_eq!(m.bytes()[0], 0, \"data was written before the trap\");\n    \
         }}"
    );
    let bin = compile("shared_elem_before_data", wat, &extra);
    let run = Command::new(&bin).output().expect("run generated binary");
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn a_trapping_defined_table_element_leaves_an_imported_shared_memory_untouched() {
    // A defined table's elements are applied while the instance is built, which
    // must still precede the data written into the host's shared memory.
    let wat = r#"
        (module
          (import "env" "mem" (memory 1 1 shared))
          (table 1 funcref)
          (elem (i32.const 5) $f)
          (data (i32.const 0) "\01")
          (func $f))
        "#;
    let extra = "fn main() {\n    \
         let m = SharedMemory::new(65536);\n    \
         let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| Instance::new(m.clone())));\n    \
         assert!(r.is_err(), \"the element should trap\");\n    \
         assert_eq!(m.bytes()[0], 0, \"data was written before the trap\");\n    \
         }";
    let bin = compile("shared_defined_table_elem_before_data", wat, extra);
    let run = Command::new(&bin).output().expect("run generated binary");
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
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

// An imported table (table 0, host-owned) next to a defined one (table 1, a field).
const IMPORTED_AND_DEFINED: &str = r#"
    (module
      (type $ft (func (result i32)))
      (import "env" "tbl" (table $t0 2 funcref))
      (table $t1 3 funcref)
      (func $a (result i32) i32.const 1)
      (func $b (result i32) i32.const 2)
      (func $c (result i32) i32.const 3)
      (elem (table $t0) (i32.const 0) func $a $b)
      (elem (table $t1) (i32.const 1) func $c $a)
      (func (export "via0") (param i32) (result i32)
        local.get 0 call_indirect $t0 (type $ft))
      (func (export "via1") (param i32) (result i32)
        local.get 0 call_indirect $t1 (type $ft))
      (func (export "size1") (result i32) table.size $t1)
      (func (export "copy_0_to_1") (param i32 i32 i32)
        local.get 0 local.get 1 local.get 2 table.copy $t1 $t0)
      (func (export "copy_1_to_0") (param i32 i32 i32)
        local.get 0 local.get 1 local.get 2 table.copy $t0 $t1))
    "#;

#[test]
fn an_imported_table_may_sit_next_to_a_defined_one() {
    // `$a $b $c` are func0..2; via0 func3, via1 func4, size1 func5, copies func6/7.
    // Table 0 is the host's, `[a, b]`; table 1 is the instance's, `[null, c, a]`.
    let extra = format!(
        "{HOST_TABLE}\n\
         fn main() {{\n    \
         let host = Host {{ table: vec![u32::MAX; 2] }};\n    \
         let mut inst = Instance::new(host);\n    \
         assert_eq!(inst.func3(0), 1);\n    \
         assert_eq!(inst.func3(1), 2);\n    \
         assert_eq!(inst.func4(1), 3);\n    \
         assert_eq!(inst.func4(2), 1);\n    \
         assert_eq!(inst.func5(), 3);\n    \
         // table.copy $t1 <- $t0: slots 0..2 of the host table land in table 1.\n    \
         inst.func6(0, 0, 2);\n    \
         assert_eq!(inst.func4(0), 1);\n    \
         assert_eq!(inst.func4(1), 2);\n    \
         // table.copy $t0 <- $t1: table 1 slot 2 lands in host slot 0.\n    \
         inst.func7(0, 2, 1);\n    \
         assert_eq!(inst.func3(0), 1);\n    \
         assert_eq!(inst.table().len(), 2);\n    \
         }}"
    );
    let bin = compile("with_defined", IMPORTED_AND_DEFINED, &extra);
    let run = Command::new(&bin).status().expect("run generated binary");
    assert!(run.success(), "imported table next to a defined one failed");
}

#[test]
fn only_the_imported_table_is_lent_through_the_host_trait() {
    let wasm = wat::parse_str(IMPORTED_AND_DEFINED).expect("valid wat");
    let generated = wasm2rs::transpile(&wasm).expect("transpile ok");
    let trait_start = generated.find("pub trait Imports").expect("trait");
    let trait_end = trait_start + generated[trait_start..].find("\n}").expect("end of trait");
    let host_trait = &generated[trait_start..trait_end];
    assert!(host_trait.contains("fn table(&self)"), "{host_trait}");
    assert!(!host_trait.contains("table1"), "{host_trait}");
    assert!(generated.contains("table1: Vec<u32>"), "{generated}");
}

#[test]
fn table_init_grow_and_fill_act_on_the_defined_table_not_the_imported_one() {
    // func0 is `$a`; exports are func1..: init1, grow1, fill1, null1, size0.
    let wat = r#"
        (module
          (import "env" "tbl" (table $t0 2 funcref))
          (table $t1 1 funcref)
          (func $a (result i32) i32.const 1)
          (elem $p funcref (ref.func $a) (ref.func $a))
          (func (export "init1") (table.init $t1 $p (i32.const 0) (i32.const 0) (i32.const 1)))
          (func (export "grow1") (param i32) (result i32)
            (table.grow $t1 (ref.func $a) (local.get 0)))
          (func (export "fill1") (param i32 i32)
            (table.fill $t1 (local.get 0) (ref.func $a) (local.get 1)))
          (func (export "null1") (param i32) (result i32)
            (ref.is_null (table.get $t1 (local.get 0))))
          (func (export "size0") (result i32) (table.size $t0)))
        "#;
    let extra = format!(
        "{HOST_TABLE}\n\
         fn main() {{\n    \
         let host = Host {{ table: vec![u32::MAX; 2] }};\n    \
         let mut inst = Instance::new(host);\n    \
         assert_eq!(inst.func4(0), 1);\n    \
         inst.func1();\n    \
         assert_eq!(inst.func4(0), 0);\n    \
         assert_eq!(inst.func2(2), 1);\n    \
         inst.func3(1, 2);\n    \
         assert_eq!(inst.func4(2), 0);\n    \
         assert_eq!(inst.func5(), 2);\n    \
         assert!(inst.table().iter().all(|&s| s == u32::MAX));\n    \
         }}"
    );
    let bin = compile("defined_ops", wat, &extra);
    let run = Command::new(&bin).status().expect("run generated binary");
    assert!(run.success(), "table ops next to an imported table failed");
}

#[test]
fn active_elements_of_an_imported_and_a_defined_table_apply_in_module_order() {
    // Segment 0 fills the host table, segment 1 (defined table) is out of bounds and
    // traps. The spec applies segments in module order, so the host table keeps
    // segment 0's write.
    let wat = r#"
        (module
          (import "env" "tbl" (table $t0 2 funcref))
          (table $t1 2 funcref)
          (func $a)
          (elem (table $t0) (i32.const 0) func $a)
          (elem (table $t1) (i32.const 5) func $a))
        "#;
    let extra = r#"
        struct Host { table: *mut Vec<u32> }
        impl Imports for Host {
            fn table(&self) -> &[u32] { unsafe { &*self.table } }
            fn table_mut(&mut self) -> &mut Vec<u32> { unsafe { &mut *self.table } }
        }
        fn main() {
            let table: &'static mut Vec<u32> = Box::leak(Box::new(vec![u32::MAX; 2]));
            let ptr: *mut Vec<u32> = table;
            std::panic::set_hook(Box::new(|_| {}));
            let r = std::panic::catch_unwind(|| { let _ = Instance::new(Host { table: ptr }); });
            assert!(r.is_err(), "the out-of-bounds segment must trap");
            assert_eq!(unsafe { &*ptr }[0], 0, "segment 0 ran before the trap");
        }
    "#;
    let bin = compile("elem_order", wat, extra);
    let run = Command::new(&bin).status().expect("run generated binary");
    assert!(
        run.success(),
        "elem segments were not applied in module order"
    );
}

const TWO_IMPORTED_AND_DEFINED: &str = r#"
    (module
      (type $ft (func (result i32)))
      (import "env" "t0" (table $t0 2 funcref))
      (import "env" "t1" (table $t1 3 funcref))
      (table $t2 1 funcref)
      (func $a (result i32) i32.const 1)
      (func $b (result i32) i32.const 2)
      (func $c (result i32) i32.const 3)
      (elem (table $t0) (i32.const 0) func $a $b)
      (elem (table $t1) (i32.const 1) func $c $a)
      (elem (table $t2) (i32.const 0) func $b)
      (func (export "via0") (param i32) (result i32)
        local.get 0 call_indirect $t0 (type $ft))
      (func (export "via1") (param i32) (result i32)
        local.get 0 call_indirect $t1 (type $ft))
      (func (export "via2") (param i32) (result i32)
        local.get 0 call_indirect $t2 (type $ft))
      (func (export "size1") (result i32) table.size $t1)
      (func (export "grow1") (result i32) ref.null func i32.const 2 table.grow $t1)
      (func (export "copy_0_to_1") (param i32 i32 i32)
        local.get 0 local.get 1 local.get 2 table.copy $t1 $t0))
    "#;

const HOST_TWO_TABLES: &str = r#"
    struct Host { t0: Vec<u32>, t1: Vec<u32> }
    impl Imports for Host {
        fn table(&self) -> &[u32] { &self.t0 }
        fn table_mut(&mut self) -> &mut Vec<u32> { &mut self.t0 }
        fn table1(&self) -> &[u32] { &self.t1 }
        fn table1_mut(&mut self) -> &mut Vec<u32> { &mut self.t1 }
    }
"#;

#[test]
fn two_imported_tables_are_each_lent_through_the_host_trait() {
    // `$a $b $c` are func0..2; via0..2 func3..5, size1 func6, grow1 func7, copy func8.
    // Host table 0 `[a, b]`, host table 1 `[null, c, a]`, defined table 2 `[b]`.
    let extra = format!(
        "{HOST_TWO_TABLES}\n\
         fn main() {{\n    \
         let host = Host {{ t0: vec![u32::MAX; 2], t1: vec![u32::MAX; 3] }};\n    \
         let mut inst = Instance::new(host);\n    \
         assert_eq!(inst.func3(0), 1);\n    \
         assert_eq!(inst.func3(1), 2);\n    \
         assert_eq!(inst.func4(1), 3);\n    \
         assert_eq!(inst.func4(2), 1);\n    \
         assert_eq!(inst.func5(0), 2);\n    \
         assert_eq!(inst.func6(), 3);\n    \
         inst.func8(0, 0, 2);\n    \
         assert_eq!(inst.func4(0), 1);\n    \
         assert_eq!(inst.func4(1), 2);\n    \
         assert_eq!(inst.func7(), 3);\n    \
         assert_eq!(inst.func6(), 5);\n    \
         assert_eq!(inst.table().len(), 2);\n    \
         assert_eq!(inst.table1().len(), 5);\n    \
         }}"
    );
    let bin = compile("two_imported", TWO_IMPORTED_AND_DEFINED, &extra);
    let run = Command::new(&bin).status().expect("run generated binary");
    assert!(run.success(), "two imported tables failed");
}

#[test]
fn a_trapping_element_of_the_second_imported_table_leaves_earlier_writes() {
    // Segment 0 fits host table 0; segment 1 is out of bounds for host table 1, so
    // instantiation traps, but table 0 keeps segment 0's write (module order).
    let wat = r#"
        (module
          (import "env" "t0" (table 1 funcref))
          (import "env" "t1" (table 1 funcref))
          (func $a)
          (elem (table 0) (i32.const 0) func $a)
          (elem (table 1) (i32.const 1) func $a))
        "#;
    let extra = r#"
        struct Host { t0: *mut Vec<u32>, t1: *mut Vec<u32> }
        impl Imports for Host {
            fn table(&self) -> &[u32] { unsafe { &*self.t0 } }
            fn table_mut(&mut self) -> &mut Vec<u32> { unsafe { &mut *self.t0 } }
            fn table1(&self) -> &[u32] { unsafe { &*self.t1 } }
            fn table1_mut(&mut self) -> &mut Vec<u32> { unsafe { &mut *self.t1 } }
        }
        fn main() {
            let t0: &'static mut Vec<u32> = Box::leak(Box::new(vec![u32::MAX; 1]));
            let t1: &'static mut Vec<u32> = Box::leak(Box::new(vec![u32::MAX; 1]));
            let (p0, p1): (*mut Vec<u32>, *mut Vec<u32>) = (t0, t1);
            std::panic::set_hook(Box::new(|_| {}));
            let r = std::panic::catch_unwind(|| { let _ = Instance::new(Host { t0: p0, t1: p1 }); });
            assert!(r.is_err(), "the out-of-bounds segment must trap");
            assert_eq!(unsafe { &*p0 }[0], 0, "segment 0 ran before the trap");
            assert_eq!(unsafe { &*p1 }[0], u32::MAX, "segment 1 wrote nothing");
        }
    "#;
    let bin = compile("two_imported_trap", wat, extra);
    let run = Command::new(&bin).status().expect("run generated binary");
    assert!(
        run.success(),
        "elem segments were not applied in module order"
    );
}

#[test]
fn an_imported_table64_is_indexed_and_sized_by_i64() {
    // A 64-bit imported table keeps the host's `Vec<u32>` storage; the module sees
    // `i64` indices and sizes. Funcs: `$a`=0, call=1, size=2, grow=3.
    let wat = r#"
        (module
          (type $ft (func (result i32)))
          (import "env" "tbl" (table $t i64 2 funcref))
          (elem (table $t) (i64.const 1) func $a)
          (func $a (result i32) i32.const 7)
          (func (export "call") (param i64) (result i32)
            local.get 0 call_indirect $t (type $ft))
          (func (export "size") (result i64) table.size $t)
          (func (export "grow") (result i64) ref.null func i64.const 3 table.grow $t))
        "#;
    let extra = format!(
        "{HOST_TABLE}\n\
         fn main() {{\n    \
         let host = Host {{ table: vec![u32::MAX; 2] }};\n    \
         let mut inst = Instance::new(host);\n    \
         assert_eq!(inst.func1(1), 7);\n    \
         assert_eq!(inst.func2(), 2);\n    \
         assert_eq!(inst.func3(), 2);\n    \
         assert_eq!(inst.func2(), 5);\n    \
         assert_eq!(inst.table().len(), 5);\n    \
         let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| inst.func1(1 << 32)));\n    \
         assert!(r.is_err(), \"an index above u32::MAX must trap\");\n    \
         }}"
    );
    let bin = compile("table64", wat, &extra);
    let run = Command::new(&bin).status().expect("run generated binary");
    assert!(run.success(), "imported table64 failed");
}
