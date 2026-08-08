//! Integration tests for cross-instance linking, Phase L2 (inbound dynamic
//! external funcref).
//!
//! A funcref is represented as a `u32`. Values with the high bit clear are this
//! module's own function indices; the high bit (`0x8000_0000`) tags an *external
//! handle* that the host resolves. When `call_indirect` reads a tagged entry it
//! strips the tag and calls `self.imports.call_ref_t{ti}(slot, args)`, whose
//! host implementation resolves the slot to another instance's function. The
//! trait method has a trapping default so existing hosts that never place a
//! tagged handle keep compiling unchanged. `u32::MAX` (null) still traps.
//!
//! Two modules are transpiled separately into sibling `mod a`/`mod b`; the host
//! fills A's imported table with a tagged handle pointing at B. Compiled with
//! `rustc -D warnings` and run.

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

fn compile(test: &str, modules: &[(&str, &str)], extra: &str) -> std::path::PathBuf {
    let mut program = String::new();
    for (name, wat) in modules {
        let wasm = wat::parse_str(wat).expect("valid wat");
        let generated = wasm2rs::transpile(&wasm).expect("transpile ok");
        program.push_str(&format!("pub mod {name} {{\n{generated}\n}}\n"));
    }
    program.push_str(extra);
    program.push('\n');

    let dir = std::env::temp_dir().join(format!("wasm2rs_external_funcref_{test}"));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let src = dir.join("gen.rs");
    let bin = dir.join(if cfg!(windows) { "gen.exe" } else { "gen" });
    std::fs::write(&src, &program).expect("write source");

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

fn run_ok(test: &str, modules: &[(&str, &str)], extra: &str) {
    let bin = compile(test, modules, extra);
    let run = Command::new(&bin).status().expect("run generated binary");
    assert!(
        run.success(),
        "generated program assertions failed:\n{test}"
    );
}

fn expect_trap(test: &str, modules: &[(&str, &str)], extra: &str) {
    let bin = compile(test, modules, extra);
    let run = Command::new(&bin).output().expect("run generated binary");
    assert!(!run.status.success(), "expected a trap:\n{test}");
}

// B: a stateless function the host will expose as an external funcref.
const B_TRIPLE: &str = r#"
    (module
      (func (export "triple") (param i32) (result i32)
        (i32.mul (local.get 0) (i32.const 3))))
    "#;

// A: imports a table and dispatches slot 0 through call_indirect (type 0).
const A_IMPORTED_TABLE: &str = r#"
    (module
      (type $sig (func (param i32) (result i32)))
      (import "env" "tbl" (table 1 funcref))
      (func $call (param i32) (result i32)
        (call_indirect (type $sig) (local.get 0) (i32.const 0))))
    "#;

#[test]
fn external_handle_dispatches_into_another_module() {
    // The host seeds A's table with a tagged handle (slot 0) and resolves it to
    // B's `triple`. A's call_indirect must route through the host to reach B.
    let extra = r#"
        struct AHost { table: Vec<u32> }
        impl a::Imports for AHost {
            fn table(&self) -> &[u32] { &self.table }
            fn table_mut(&mut self) -> &mut Vec<u32> { &mut self.table }
            fn call_ref_t0(&mut self, f: u32, a0: i32) -> i32 {
                match f { 0 => b::func0(a0), _ => panic!("bad external handle") }
            }
        }
        fn main() {
            let mut inst = a::Instance::new(AHost { table: vec![0x8000_0000u32] });
            assert_eq!(inst.func0(5), 15);
        }
    "#;
    run_ok("basic", &[("a", A_IMPORTED_TABLE), ("b", B_TRIPLE)], extra);
}

#[test]
fn local_and_external_funcrefs_coexist_in_one_table() {
    // A defines its own `double` and imports a 2-slot table. Slot 0 is a tagged
    // external handle (B's triple); slot 1 is A's own local index 0 (tag clear).
    let a = r#"
        (module
          (type $sig (func (param i32) (result i32)))
          (import "env" "tbl" (table 2 funcref))
          (func $double (param i32) (result i32) (i32.mul (local.get 0) (i32.const 2)))
          (func $call (param i32 i32) (result i32)
            (call_indirect (type $sig) (local.get 1) (local.get 0))))
        "#;
    let extra = r#"
        struct AHost { table: Vec<u32> }
        impl a::Imports for AHost {
            fn table(&self) -> &[u32] { &self.table }
            fn table_mut(&mut self) -> &mut Vec<u32> { &mut self.table }
            fn call_ref_t0(&mut self, f: u32, a0: i32) -> i32 {
                match f { 0 => b::func0(a0), _ => panic!("bad external handle") }
            }
        }
        fn main() {
            // slot 0 = external(B triple); slot 1 = local index 0 ($double).
            let mut inst = a::Instance::new(AHost { table: vec![0x8000_0000u32, 0] });
            assert_eq!(inst.func1(0, 5), 15); // external → B triple
            assert_eq!(inst.func1(1, 5), 10); // local → A double
        }
    "#;
    run_ok("coexist", &[("a", a), ("b", B_TRIPLE)], extra);
}

#[test]
fn external_handle_drives_state_in_another_instance() {
    // B keeps a counter (an Instance the host owns); each dispatch through A's
    // external handle advances it — a genuinely dynamic cross-instance funcref.
    let b = r#"
        (module
          (global $c (mut i32) (i32.const 0))
          (func (export "next") (result i32)
            (global.set $c (i32.add (global.get $c) (i32.const 1)))
            (global.get $c)))
        "#;
    let a = r#"
        (module
          (type $sig (func (result i32)))
          (import "env" "tbl" (table 1 funcref))
          (func $call (result i32) (call_indirect (type $sig) (i32.const 0))))
        "#;
    let extra = r#"
        struct AHost { table: Vec<u32>, b: b::Instance }
        impl a::Imports for AHost {
            fn table(&self) -> &[u32] { &self.table }
            fn table_mut(&mut self) -> &mut Vec<u32> { &mut self.table }
            fn call_ref_t0(&mut self, f: u32) -> i32 {
                match f { 0 => self.b.func0(), _ => panic!("bad external handle") }
            }
        }
        fn main() {
            let mut inst = a::Instance::new(AHost {
                table: vec![0x8000_0000u32],
                b: b::Instance::new(),
            });
            assert_eq!(inst.func0(), 1);
            assert_eq!(inst.func0(), 2);
            assert_eq!(inst.func0(), 3);
        }
    "#;
    run_ok("stateful", &[("a", a), ("b", b)], extra);
}

#[test]
fn null_entry_still_traps_and_does_not_reach_the_host() {
    // `u32::MAX` is null: it must trap in-module, not be forwarded as an
    // external handle (its high bit is set too).
    let extra = r#"
        struct AHost { table: Vec<u32> }
        impl a::Imports for AHost {
            fn table(&self) -> &[u32] { &self.table }
            fn table_mut(&mut self) -> &mut Vec<u32> { &mut self.table }
            fn call_ref_t0(&mut self, _f: u32, _a0: i32) -> i32 { 999 }
        }
        fn main() {
            let mut inst = a::Instance::new(AHost { table: vec![u32::MAX] });
            inst.func0(5);
        }
    "#;
    expect_trap("null", &[("a", A_IMPORTED_TABLE), ("b", B_TRIPLE)], extra);
}
