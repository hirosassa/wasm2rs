//! Integration tests for cross-instance linking, Phase L1 (outbound dispatch).
//!
//! The `call_indirect` dispatch table is exposed as a public method
//! `call_ref_t{typeidx}(&mut self, f: u32, args..) -> res`, one per call_indirect
//! signature. A module hands a `funcref` (a `u32`) out to the host (here through
//! an exported `table.get`), and the host invokes it back on the instance
//! through that public method — the outbound half of cross-instance funcref.
//! The generated Rust is compiled with `rustc -D warnings` and run.

use std::process::Command;

fn compile(test: &str, wat: &str, extra: &str) -> std::path::PathBuf {
    let wasm = wat::parse_str(wat).expect("valid wat");
    let generated = wasm2rs::transpile(&wasm).expect("transpile ok");

    let dir = std::env::temp_dir().join(format!("wasm2rs_funcref_dispatch_{test}"));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let src = dir.join("gen.rs");
    let bin = dir.join(if cfg!(windows) { "gen.exe" } else { "gen" });
    std::fs::write(&src, format!("{generated}\n{extra}\n")).expect("write source");

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
        "generated code failed to compile:\n--- stderr ---\n{}",
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

// Two local functions in a table, an exported getter that returns a funcref,
// and a `call_indirect` that fixes the dispatch signature (type 0).
const TABLE_M: &str = r#"
    (module
      (type $sig (func (param i32) (result i32)))
      (table 2 funcref)
      (elem (i32.const 0) $inc $dbl)
      (func $inc (param i32) (result i32) (i32.add (local.get 0) (i32.const 1)))
      (func $dbl (param i32) (result i32) (i32.mul (local.get 0) (i32.const 2)))
      (func $get (param i32) (result funcref) (table.get (local.get 0)))
      (func $call (param i32 i32) (result i32)
        (call_indirect (type $sig) (local.get 1) (local.get 0))))
    "#;

#[test]
fn host_invokes_a_funcref_handed_out_by_the_module() {
    // The host reads two funcrefs out of the table and calls each one back
    // through the generated `call_ref_t0` dispatch method.
    let extra = r#"
        fn main() {
            let mut inst = Instance::new();
            let f_inc = inst.func2(0);
            let f_dbl = inst.func2(1);
            assert_eq!(inst.call_ref_t0(f_inc, 5), 6);
            assert_eq!(inst.call_ref_t0(f_dbl, 5), 10);
        }
    "#;
    run_ok("outbound", TABLE_M, extra);
}

#[test]
fn call_indirect_still_dispatches_through_the_shared_method() {
    // The module's own `call_indirect` must keep working (it now delegates to
    // the same `call_ref_t0` method).
    let extra = r#"
        fn main() {
            let mut inst = Instance::new();
            assert_eq!(inst.func3(0, 5), 6);
            assert_eq!(inst.func3(1, 5), 10);
        }
    "#;
    run_ok("internal", TABLE_M, extra);
}

#[test]
fn dispatching_a_null_funcref_traps() {
    // A null funcref (`u32::MAX`) matches no arm and hits the type-mismatch trap.
    let extra = r#"
        fn main() {
            let mut inst = Instance::new();
            inst.call_ref_t0(u32::MAX, 5);
        }
    "#;
    expect_trap("null", TABLE_M, extra);
}
