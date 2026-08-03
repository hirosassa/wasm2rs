//! Integration tests for "Tier 2" tables: a host-native (imported) function
//! placed in a table and reached through `call_indirect`. An imported function
//! occupies the low indices of the same `u32` funcref index space as the
//! module's own functions, so a table entry can be an imported index and
//! `call_indirect` dispatches it through the injected host trait
//! (`self.imports.import{n}(..)`) — no separate fn-pointer/trait-object
//! mechanism is needed. These tests lock that in end to end: the generated Rust
//! is compiled with `rustc -D warnings` and exercised.
//!
//! The genuinely-unimplemented remainder of Tier 2 is a funcref from *another*
//! module instance (cross-instance dispatch / module linking), which a
//! single-module `transpile` cannot even express; it is out of scope here.

use std::process::Command;

fn compile(test: &str, wat: &str, extra: &str) -> std::path::PathBuf {
    let wasm = wat::parse_str(wat).expect("valid wat");
    let generated = wasm2rs::transpile(&wasm).expect("transpile ok");

    let dir = std::env::temp_dir().join(format!("wasm2rs_tier2_{test}"));
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

fn run_ok(test: &str, wat: &str, extra: &str) {
    let bin = compile(test, wat, extra);
    let run = Command::new(&bin).status().expect("run generated binary");
    assert!(
        run.success(),
        "generated program assertions failed:\n{test}"
    );
}

fn expect_trap(test: &str, wat: &str, extra: &str) {
    let bin = compile(test, wat, extra);
    let run = Command::new(&bin).output().expect("run generated binary");
    assert!(!run.status.success(), "expected a trap:\n{test}");
}

// A host function and a local function of the same type share a defined table,
// seeded by an active element segment. `call_indirect` dispatches by index: the
// host slot routes to the host trait, the local slot to the module's method.
const MIXED_TABLE: &str = r#"
    (module
      (type $sig (func (param i32) (result i32)))
      (import "env" "host_fn" (func $host (type $sig)))
      (table 2 funcref)
      (elem (i32.const 0) $host $local)
      (func $local (param i32) (result i32) (i32.mul (local.get 0) (i32.const 10)))
      (func $dispatch (param i32 i32) (result i32)
        (call_indirect (type $sig) (local.get 1) (local.get 0))))
    "#;

const MIXED_HOST: &str = r#"
    struct Host;
    impl Imports for Host {
        fn import0(&mut self, a0: i32) -> i32 { a0 + 1000 }
    }
"#;

#[test]
fn host_and_local_functions_share_a_table() {
    let extra = format!(
        "{MIXED_HOST}\n\
         fn main() {{\n    \
         let mut inst = Instance::new(Host);\n    \
         assert_eq!(inst.func2(0, 5), 1005);\n    \
         assert_eq!(inst.func2(1, 5), 50);\n    \
         }}"
    );
    run_ok("mixed", MIXED_TABLE, &extra);
}

#[test]
fn indirect_call_past_the_table_traps() {
    let extra = format!(
        "{MIXED_HOST}\n\
         fn main() {{ let mut inst = Instance::new(Host); inst.func2(9, 5); }}"
    );
    expect_trap("mixed_oob", MIXED_TABLE, &extra);
}

// A table holding *only* a host function (the dispatcher's own signature
// differs, so the host import is the sole `call_indirect` target). The host
// funcref is installed at runtime with `ref.func` + `table.set` into a
// host-owned imported table.
const PURE_HOST_TABLE: &str = r#"
    (module
      (type $sig (func (param i32) (result i32)))
      (import "env" "host_fn" (func $host (type $sig)))
      (import "env" "tbl" (table 1 funcref))
      (func $install (table.set (i32.const 0) (ref.func $host)))
      (func $dispatch (param i32 i32) (result i32)
        (call_indirect (type $sig) (local.get 1) (local.get 0))))
    "#;

const PURE_HOST: &str = r#"
    struct Host { table: Vec<u32> }
    impl Imports for Host {
        fn table(&self) -> &[u32] { &self.table }
        fn table_mut(&mut self) -> &mut Vec<u32> { &mut self.table }
        fn import0(&mut self, a0: i32) -> i32 { a0 * 3 }
    }
"#;

#[test]
fn ref_func_installs_a_host_fn_into_an_imported_table() {
    let extra = format!(
        "{PURE_HOST}\n\
         fn main() {{\n    \
         let mut inst = Instance::new(Host {{ table: vec![u32::MAX; 1] }});\n    \
         inst.func1();\n    \
         assert_eq!(inst.func2(0, 7), 21);\n    \
         }}"
    );
    run_ok("pure_host", PURE_HOST_TABLE, &extra);
}

#[test]
fn indirect_call_on_an_uninstalled_null_slot_traps() {
    // Without calling `$install`, slot 0 is null (`u32::MAX`), which matches no
    // arm and falls through to the type-mismatch trap.
    let extra = format!(
        "{PURE_HOST}\n\
         fn main() {{\n    \
         let mut inst = Instance::new(Host {{ table: vec![u32::MAX; 1] }});\n    \
         inst.func2(0, 7);\n    \
         }}"
    );
    expect_trap("pure_host_null", PURE_HOST_TABLE, &extra);
}

// A host function seeded through a *passive* element segment plus `table.init`.
const PASSIVE_HOST_TABLE: &str = r#"
    (module
      (type $sig (func (param i32) (result i32)))
      (import "env" "host_fn" (func $host (type $sig)))
      (table 1 funcref)
      (elem func $host)
      (func $init (table.init 0 (i32.const 0) (i32.const 0) (i32.const 1)))
      (func $dispatch (param i32 i32) (result i32)
        (call_indirect (type $sig) (local.get 1) (local.get 0))))
    "#;

#[test]
fn passive_element_and_table_init_seed_a_host_fn() {
    let extra = format!(
        "{MIXED_HOST}\n\
         fn main() {{\n    \
         let mut inst = Instance::new(Host);\n    \
         inst.func1();\n    \
         assert_eq!(inst.func2(0, 4), 1004);\n    \
         }}"
    );
    run_ok("passive_host", PASSIVE_HOST_TABLE, &extra);
}
