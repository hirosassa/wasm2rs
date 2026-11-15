//! Resources a component defines and exports (`canon resource.new` / `resource.rep` /
//! `resource.drop` of the guest). The glue keeps the guest's handle table; the host holds a
//! resource as its representation (`ResourceN(rep)`), as a callee that defines a resource sees a
//! `borrow` as the representation itself. Each test encodes a component around a hand-written
//! core module with `wit-component`, appends the glue and calls the typed methods.

use std::process::Command;

use crate::common;
use wasm2rs::{
    ComponentOptions, TranspileOptions, WitOptions, transpile_component_with_exports,
    transpile_with_options, wit_host_glue_with_options,
};
use wit_component::{ComponentEncoder, StringEncoding, embed_component_metadata};
use wit_parser::Resolve;

const WIT: &str = "package t:p;
interface i {
  resource counter {
    constructor(start: u32);
    get: func() -> u32;
    peek: static func(c: counter) -> u32;
    make: static func(n: u32) -> counter;
    try-make: static func(n: u32) -> result<counter, string>;
    bad-rep: static func(c: counter) -> u32;
    spill: static func(c: counter, a: u32, b: u32, c2: u32, d: u32, e: u32, f: u32, g2: u32, h: u32, i: u32, j: u32, k: u32, l: u32, m: u32, n: u32, o: u32, p: u32) -> u32;
  }
}
world w { export i; }";

/// `get` returns its `borrow` (the representation); `peek` gets an owned handle, reads its
/// representation and drops it; the destructor counts its calls and keeps the last one.
const CORE: &str = r#"(module
  (import "[export]t:p/i" "[resource-new]counter" (func $new (param i32) (result i32)))
  (import "[export]t:p/i" "[resource-rep]counter" (func $rep (param i32) (result i32)))
  (import "[export]t:p/i" "[resource-drop]counter" (func $drop (param i32)))
  (memory (export "memory") 1)
  (global $dtors (mut i32) (i32.const 0))
  (global $last (mut i32) (i32.const 0))
  (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32) i32.const 0)
  (func (export "t:p/i#[constructor]counter") (param i32) (result i32) local.get 0 call $new)
  (func (export "t:p/i#[method]counter.get") (param i32) (result i32) local.get 0)
  (func (export "t:p/i#[static]counter.peek") (param i32) (result i32)
    (local $r i32)
    (local.set $r (call $rep (local.get 0)))
    (call $drop (local.get 0))
    local.get $r)
  (func (export "t:p/i#[static]counter.make") (param i32) (result i32) local.get 0 call $new)
  (func (export "t:p/i#[static]counter.try-make") (param i32) (result i32)
    (i32.store8 (i32.const 64) (i32.const 0))
    (i32.store offset=4 (i32.const 64) (call $new (local.get 0)))
    i32.const 64)
  (func (export "t:p/i#[static]counter.bad-rep") (param i32) (result i32)
    (call $rep (i32.add (local.get 0) (i32.const 99))))
  (func (export "t:p/i#[static]counter.spill") (param i32) (result i32)
    (call $rep (i32.load (local.get 0))))
  (func (export "t:p/i#[dtor]counter") (param i32)
    (global.set $dtors (i32.add (global.get $dtors) (i32.const 1)))
    (global.set $last (local.get 0)))
  (func (export "dtors") (result i32) global.get $dtors)
  (func (export "last") (result i32) global.get $last))"#;

fn component() -> Vec<u8> {
    let mut resolve = Resolve::default();
    let package = resolve.push_str("test.wit", WIT).unwrap();
    let world = resolve.select_world(&[package], Some("w")).unwrap();
    let mut module = wat::parse_str(CORE).unwrap();
    embed_component_metadata(&mut module, &resolve, world, StringEncoding::UTF8, false).unwrap();
    ComponentEncoder::default()
        .module(&module)
        .unwrap()
        .validate(true)
        .encode()
        .unwrap()
}

/// Builds the program (the transpiled core, the glue with exports, a `main` running `body`
/// on `inst`) and returns its output.
fn build_and_run(name: &str, body: &str) -> std::process::Output {
    let options = WitOptions {
        exports: true,
        ..WitOptions::default()
    };
    let glue = wit_host_glue_with_options(&component(), &options).unwrap();
    let transpile = TranspileOptions::default()
        .with_named_imports(true)
        .with_reentrant_imports(true)
        .with_named_exports(true);
    let generated = transpile_with_options(&wat::parse_str(CORE).unwrap(), &transpile).unwrap();
    let program = format!(
        "{generated}\n{glue}\nstruct H;\nimpl Host for H {{}}\n\
         fn main() {{ let mut inst = Instance::new(Glue::new(H)); {body} }}\n"
    );
    let bin = common::build(&format!("wit_glue_resource_exports_{name}"), &program);
    Command::new(bin).output().unwrap()
}

fn run(name: &str, body: &str) {
    let out = build_and_run(name, body);
    assert!(
        out.status.success(),
        "{name}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_constructed_resource_is_held_as_its_representation_and_borrowed_back_as_is() {
    run(
        "construct",
        "let c = Exports::t_p_i_constructor_counter(&mut inst, 7);\n\
         assert_eq!(c, Resource0(7));\n\
         assert_eq!(Exports::t_p_i_method_counter_get(&mut inst, c), 7);\n\
         let d = Exports::t_p_i_static_counter_make(&mut inst, 9);\n\
         assert_eq!(Exports::t_p_i_method_counter_get(&mut inst, d), 9);\n\
         // two live resources do not disturb each other\n\
         assert_eq!(Exports::t_p_i_method_counter_get(&mut inst, c), 7);",
    );
}

#[test]
fn an_owned_argument_becomes_a_guest_handle_and_its_drop_runs_the_destructor_after_the_call() {
    run(
        "own_param",
        "assert_eq!(Exports::t_p_i_static_counter_peek(&mut inst, Resource0(41)), 41);\n\
         assert_eq!(inst.dtors(), 1);\n\
         assert_eq!(inst.last(), 41);",
    );
}

#[test]
fn an_owned_argument_spilled_to_guest_memory_becomes_a_handle_too() {
    run(
        "own_spilled",
        "let r = Exports::t_p_i_static_counter_spill(\n\
             &mut inst, Resource0(8), 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16);\n\
         assert_eq!(r, 8);",
    );
}

#[test]
fn the_host_drops_an_owned_result_through_the_destructor() {
    run(
        "host_drop",
        "let c = Exports::t_p_i_constructor_counter(&mut inst, 5);\n\
         assert_eq!(inst.dtors(), 0);\n\
         Exports::drop_resource0(&mut inst, c);\n\
         assert_eq!((inst.dtors(), inst.last()), (1, 5));",
    );
}

#[test]
fn an_owned_handle_in_a_result_read_from_memory_is_taken_out_of_the_table() {
    run(
        "own_in_memory",
        "let r = Exports::t_p_i_static_counter_try_make(&mut inst, 3);\n\
         assert_eq!(r, Ok(Resource0(3)));",
    );
}

#[test]
fn a_handle_the_guest_never_made_traps() {
    let out = build_and_run(
        "bad_handle",
        "let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {\n\
             Exports::t_p_i_static_counter_bad_rep(&mut inst, Resource0(1))\n\
         }));\n\
         let payload = r.unwrap_err();\n\
         assert_eq!(\n\
             payload.downcast_ref::<Wasm2RsGlueTrap>(),\n\
             Some(&Wasm2RsGlueTrap::InvalidResourceHandle)\n\
         );",
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Set `WASM2RS_RESOURCE_GUEST` to the `counter` component (a `wit-bindgen` 0.63 Rust guest
/// of `wasm32-wasip2`, see `docs/component-exports-design.md`) and run with `--ignored`.
#[test]
#[ignore = "needs WASM2RS_RESOURCE_GUEST (a wit-bindgen guest that exports a resource)"]
fn a_real_wit_bindgen_guest_exporting_a_resource_is_driven_through_the_typed_exports() {
    let path = std::env::var_os("WASM2RS_RESOURCE_GUEST")
        .expect("WASM2RS_RESOURCE_GUEST names the component");
    let component = std::fs::read(path).unwrap();
    let (code, _) =
        transpile_component_with_exports(&component, &ComponentOptions::default()).unwrap();
    // The type is named after the toolchain's type index: take the method that drops it.
    let drop = code
        .split("fn drop_resource")
        .nth(1)
        .and_then(|rest| rest.split('(').next())
        .map(|name| format!("drop_resource{name}"))
        .expect("the glue has a method that drops the resource");
    let program = format!(
        "{code}\n\
         fn main() {{\n\
             let host = wasm2rs_wasi::WasiHost::new(Vec::new());\n\
             let mut inst = Instance::new(Glue::new(host));\n\
             let c = Exports::test_counter_counters_constructor_counter(&mut inst, 40);\n\
             Exports::test_counter_counters_method_counter_add(&mut inst, c, 2);\n\
             println!(\"{{}}\", Exports::test_counter_counters_method_counter_get(&mut inst, c));\n\
             let d = Exports::test_counter_counters_static_counter_make(&mut inst, 5);\n\
             println!(\"{{}}\", Exports::test_counter_counters_static_counter_sum(&mut inst, c, d));\n\
             println!(\"{{}}\", Exports::test_counter_counters_static_counter_consume(&mut inst, d));\n\
             Exports::{drop}(&mut inst, c);\n\
             for n in 0..1000 {{\n\
                 let e = Exports::test_counter_counters_constructor_counter(&mut inst, n);\n\
                 assert_eq!(Exports::test_counter_counters_method_counter_get(&mut inst, e), n);\n\
                 Exports::{drop}(&mut inst, e);\n\
             }}\n\
         }}\n"
    );
    let bin = common::build_with_wasi("real_wit_bindgen_resource", &program);
    let out = Command::new(bin).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "42\n47\n5\n");
}
