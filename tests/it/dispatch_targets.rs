//! A `call_indirect` dispatch method routes only to functions that can ever be
//! a funcref value: those named by an element segment, a `ref.func`, a global or
//! table initialiser, or an export. A function of the same signature that is
//! none of those is unreachable through the table, so it gets no `match` arm.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::string_slice,
    reason = "test code"
)]

/// The `match` arms (`{idx}u32 =>`) of the generated `call_ref_t0` method.
fn dispatch_arms(wat: &str) -> Vec<u32> {
    let wasm = wat::parse_str(wat).expect("valid wat");
    let generated = wasm2rs::transpile(&wasm).expect("transpile ok");
    let start = generated
        .find("pub fn call_ref_t0(")
        .expect("dispatch method emitted");
    let body = &generated[start..];
    let end = body.find("_ => trap_indirect_type_mismatch()").unwrap();
    body[..end]
        .lines()
        .filter_map(|l| l.trim().split_once("u32 =>"))
        .map(|(n, _)| n.parse().unwrap())
        .collect()
}

#[test]
fn unreferenced_same_signature_function_gets_no_dispatch_arm() {
    // f0: in the element segment; f1: referenced nowhere; f2: exported;
    // f3: only a `ref.func` target; f4: named by the global initialiser.
    let arms = dispatch_arms(
        r#"(module
          (type $t (func (result i32)))
          (table 4 funcref)
          (elem (i32.const 0) $f0)
          (elem declare func $f3)
          (global $g funcref (ref.func $f4))
          (func $f0 (type $t) (i32.const 10))
          (func $f1 (type $t) (i32.const 11))
          (func $f2 (type $t) (i32.const 12))
          (func $f3 (type $t) (i32.const 13))
          (func $f4 (type $t) (i32.const 14))
          (func (export "run") (param i32) (result i32)
            (table.set (i32.const 1) (ref.func $f3))
            (call_indirect (type $t) (local.get 0)))
          (export "f2" (func $f2)))"#,
    );
    assert_eq!(arms, vec![0, 2, 3, 4]);
}

#[test]
fn a_host_owned_table_keeps_every_function_of_the_signature_as_a_target() {
    // The host fills an imported table with raw function indices, so it can name
    // a function the module itself never references.
    let arms = dispatch_arms(
        r#"(module
          (type $t (func (result i32)))
          (import "env" "tbl" (table 2 funcref))
          (func $f0 (type $t) (i32.const 10))
          (func $f1 (type $t) (i32.const 11))
          (func (export "run") (param i32) (result i32)
            (call_indirect (type $t) (local.get 0))))"#,
    );
    assert_eq!(arms, vec![0, 1]);
}

#[test]
fn a_function_taking_a_funcref_parameter_keeps_every_function_as_a_target() {
    // Every defined function is a public method, so the host can pass any raw
    // funcref into `$run` even though nothing exports it.
    let arms = dispatch_arms(
        r#"(module
          (type $t (func (result i32)))
          (table 1 funcref)
          (func $f0 (type $t) (i32.const 10))
          (func $f1 (type $t) (i32.const 11))
          (func $run (param funcref) (result i32)
            (table.set (i32.const 0) (local.get 0))
            (call_indirect (type $t) (i32.const 0))))"#,
    );
    assert_eq!(arms, vec![0, 1]);
}
