//! Integration tests for cross-instance linking, Phase L3 (Linker helper).
//!
//! For every `call_indirect` signature, an external-funcref module receives a
//! generated `ExternFuncs{ti}` registry: `register(boxed closure) -> handle`
//! hands back a tagged funcref to store in a table, and `call(slot, args)`
//! resolves a stripped slot (as delivered to `Imports::call_ref_t{ti}`) back to
//! its closure. This removes the host's need to manage slot numbers or the tag
//! bit by hand. Two modules are transpiled into sibling `mod a`/`mod b`;
//! compiled with `rustc -D warnings` and run.

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

    let dir = std::env::temp_dir().join(format!("wasm2rs_extern_registry_{test}"));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let src = dir.join("gen.rs");
    let bin = dir.join(if cfg!(windows) { "gen.exe" } else { "gen" });
    std::fs::write(&src, &program).expect("write source");

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

fn run_ok(test: &str, modules: &[(&str, &str)], extra: &str) {
    let bin = compile(test, modules, extra);
    let run = Command::new(&bin).status().expect("run generated binary");
    assert!(
        run.success(),
        "generated program assertions failed:\n{test}"
    );
}

const B_TRIPLE: &str = r#"
    (module
      (func (export "triple") (param i32) (result i32)
        (i32.mul (local.get 0) (i32.const 3))))
    "#;

const A_IMPORTED_TABLE: &str = r#"
    (module
      (type $sig (func (param i32) (result i32)))
      (import "env" "tbl" (table 1 funcref))
      (func $call (param i32) (result i32)
        (call_indirect (type $sig) (local.get 0) (i32.const 0))))
    "#;

#[test]
fn registry_links_a_stateless_function() {
    // Register B's `triple` in the generated registry, store the returned handle
    // in A's table, and dispatch it back through `ExternFuncs0::call`.
    let extra = r#"
        struct AHost { table: Vec<u32>, ext0: a::ExternFuncs0 }
        impl a::Imports for AHost {
            fn table(&self) -> &[u32] { &self.table }
            fn table_mut(&mut self) -> &mut Vec<u32> { &mut self.table }
            fn call_ref_t0(&mut self, slot: u32, a0: i32) -> i32 { self.ext0.call(slot, a0) }
        }
        fn main() {
            let mut ext0 = a::ExternFuncs0::new();
            let handle = ext0.register(Box::new(|x| b::func0(x)));
            let mut inst = a::Instance::new(AHost { table: vec![handle], ext0 });
            assert_eq!(inst.func0(5), 15);
        }
    "#;
    run_ok(
        "stateless",
        &[("a", A_IMPORTED_TABLE), ("b", B_TRIPLE)],
        extra,
    );
}

#[test]
fn registry_closure_owns_a_stateful_instance() {
    // The registered closure captures a B `Instance`; each dispatch advances its
    // counter — cross-instance state managed entirely through the registry.
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
        struct AHost { table: Vec<u32>, ext0: a::ExternFuncs0 }
        impl a::Imports for AHost {
            fn table(&self) -> &[u32] { &self.table }
            fn table_mut(&mut self) -> &mut Vec<u32> { &mut self.table }
            fn call_ref_t0(&mut self, slot: u32) -> i32 { self.ext0.call(slot) }
        }
        fn main() {
            let mut ext0 = a::ExternFuncs0::new();
            let mut b_inst = b::Instance::new();
            let handle = ext0.register(Box::new(move || b_inst.func0()));
            let mut inst = a::Instance::new(AHost { table: vec![handle], ext0 });
            assert_eq!(inst.func0(), 1);
            assert_eq!(inst.func0(), 2);
        }
    "#;
    run_ok("stateful", &[("a", a), ("b", b)], extra);
}

#[test]
fn registry_hands_out_distinct_handles() {
    // Two registrations get distinct handles addressing distinct closures.
    let b = r#"
        (module
          (func (export "inc") (param i32) (result i32) (i32.add (local.get 0) (i32.const 1)))
          (func (export "dec") (param i32) (result i32) (i32.sub (local.get 0) (i32.const 1))))
        "#;
    let a = r#"
        (module
          (type $sig (func (param i32) (result i32)))
          (import "env" "tbl" (table 2 funcref))
          (func $call (param i32 i32) (result i32)
            (call_indirect (type $sig) (local.get 1) (local.get 0))))
        "#;
    let extra = r#"
        struct AHost { table: Vec<u32>, ext0: a::ExternFuncs0 }
        impl a::Imports for AHost {
            fn table(&self) -> &[u32] { &self.table }
            fn table_mut(&mut self) -> &mut Vec<u32> { &mut self.table }
            fn call_ref_t0(&mut self, slot: u32, a0: i32) -> i32 { self.ext0.call(slot, a0) }
        }
        fn main() {
            let mut ext0 = a::ExternFuncs0::new();
            let h_inc = ext0.register(Box::new(|x| b::func0(x)));
            let h_dec = ext0.register(Box::new(|x| b::func1(x)));
            let mut inst = a::Instance::new(AHost { table: vec![h_inc, h_dec], ext0 });
            assert_eq!(inst.func0(0, 10), 11); // slot 0 → inc
            assert_eq!(inst.func0(1, 10), 9); // slot 1 → dec
        }
    "#;
    run_ok("distinct", &[("a", a), ("b", b)], extra);
}
