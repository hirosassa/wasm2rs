//! A dylink-shaped component (componentize-py's layout in miniature): `main` defines the memory
//! and table and exports each library's `__memory_base` / `__table_base`, the libraries import
//! them and fill the shared table and memory at those bases. The linker folds the bases to
//! constants, so the libraries' segments land at the right addresses.

use std::process::Command;

use wasm2rs::{ComponentOptions, transpile_component};

use crate::common;

fn own_host() -> ComponentOptions {
    ComponentOptions {
        external_types: None,
        ..ComponentOptions::default()
    }
}

/// What a library imports: the shared memory and table and its two bases.
const LIB_IMPORTS: &str = r#"
    (import "env" "memory" (memory 1))
    (import "env" "__indirect_function_table" (table 16 funcref))
    (import "env" "__table_base" (global $tb i32))
    (import "env" "__memory_base" (global $mb i32))"#;

/// A component that lifts only from `main` would be transpiled as `main` alone, without its
/// libraries, so every component here also lifts a no-op from the first library.
///
/// `libs` are the bodies of the library modules, `main_extra` more items of `main`; `lifts`
/// are the component's `canon lift`s and exports (`$m` is main's instance, `$l0`, `$l1` ... the
/// libraries'). Library `i` gets `__table_base` 7 + 2i and `__memory_base` 64 (i + 1).
fn component(main_extra: &str, libs: &[&str], init: &str, lifts: &str) -> Vec<u8> {
    let mut bases = String::new();
    for i in 0..libs.len() {
        bases.push_str(&format!(
            "(global (export \"lib{i}:table_base\") i32 (i32.const {}))\n\
             (global (export \"lib{i}:memory_base\") i32 (i32.const {}))\n",
            7 + 2 * i,
            64 * (i + 1)
        ));
    }
    let mut modules = String::new();
    let mut instances = String::new();
    for (i, body) in libs.iter().enumerate() {
        modules.push_str(&format!(
            "(core module $lib{i} {LIB_IMPORTS}\n{body}\n\
             (func (export \"nop\") (result i32) (i32.const 0)))\n"
        ));
        instances.push_str(&format!(
            "(alias core export $m \"lib{i}:table_base\" (core global $tb{i}))\n\
             (alias core export $m \"lib{i}:memory_base\" (core global $mb{i}))\n\
             (core instance $env{i} (export \"memory\" (memory $mem))\n\
                 (export \"__indirect_function_table\" (table $tbl))\n\
                 (export \"__table_base\" (global $tb{i}))\n\
                 (export \"__memory_base\" (global $mb{i})))\n\
             (core instance $l{i} (instantiate $lib{i} (with \"env\" (instance $env{i}))))\n"
        ));
    }
    let init_module = if init.is_empty() {
        String::new()
    } else {
        format!("(core module $init (import \"env\" \"memory\" (memory 1))\n{init})\n")
    };
    let init_instance = if init.is_empty() {
        ""
    } else {
        "(core instance $envi (export \"memory\" (memory $mem)))\n\
         (core instance $i0 (instantiate $init (with \"env\" (instance $envi))))\n"
    };
    let wat = format!(
        r#"(component
    (import "t:t/i" (instance $i (export "f" (func (result u32)))))
    (alias export $i "f" (func $hf))
    (core func $lowered (canon lower (func $hf)))
    (core instance $host (export "f" (func $lowered)))
    (core module $main
        (import "t:t/i" "f" (func (result i32)))
        (memory (export "memory") 1)
        (table (export "table") 16 funcref)
        {bases}
        (type $t (func (result i32)))
        (func (export "call_at") (param i32) (result i32) (call_indirect (type $t) (local.get 0)))
        (func (export "peek") (param i32) (result i32) (i32.load8_u (local.get 0)))
        {main_extra})
    {modules}{init_module}
    (core instance $m (instantiate $main (with "t:t/i" (instance $host))))
    (alias core export $m "memory" (core memory $mem))
    (alias core export $m "table" (core table $tbl))
    {instances}{init_instance}
    (func $nop (result s32) (canon lift (core func $l0 "nop")))
    (export "nop" (func $nop))
    {lifts})"#
    );
    wat::parse_str(wat).unwrap()
}

const LIFT_CALL_AT: &str = r#"
    (func $call_at (param "i" s32) (result s32) (canon lift (core func $m "call_at")))
    (export "call-at" (func $call_at))"#;
const LIFT_PEEK: &str = r#"
    (func $peek (param "a" s32) (result s32) (canon lift (core func $m "peek")))
    (export "peek" (func $peek))"#;

fn run(name: &str, component: &[u8], main: &str) -> String {
    let code = transpile_component(component, &own_host()).unwrap();
    let program = format!(
        "{code}\n\
         struct H;\n\
         impl Host for H {{ fn t_t_i_f(&mut self) -> u32 {{ 0 }} }}\n\
         fn main() {{\n\
             let mut inst = Instance::new(Glue(H));\n\
             {main}\n\
         }}\n"
    );
    let bin = common::build(name, &program);
    let out = Command::new(bin).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// (a) A library's `i32.trunc_sat_f64_s` links and saturates.
#[test]
fn a_library_may_use_saturating_float_to_int() {
    let lib = r#"(func (export "sat") (result i32) (i32.trunc_sat_f64_s (f64.const 1e30)))"#;
    let lifts = r#"
    (func $sat (result s32) (canon lift (core func $l0 "sat")))
    (export "sat" (func $sat))"#;
    let out = run(
        "dylink_trunc_sat",
        &component("", &[lib], "", lifts),
        "println!(\"{}\", Exports::sat(&mut inst));",
    );
    assert_eq!(out, "2147483647");
}

/// (b) A library fills the shared table at its `__table_base` (an imported global `main`
/// defines as `i32.const 7`), and `main` calls through it.
#[test]
fn a_library_fills_the_shared_table_at_its_table_base() {
    let lib = r#"(func $g (result i32) i32.const 77)
        (elem (global.get $tb) $g)"#;
    let out = run(
        "dylink_table_base",
        &component("", &[lib], "", LIFT_CALL_AT),
        "println!(\"{}\", Exports::call_at(&mut inst, 7));",
    );
    assert_eq!(out, "77");
}

/// (c) A library's active data segment lands at its `__memory_base`.
#[test]
fn a_library_places_its_data_at_its_memory_base() {
    let lib = r#"(data (global.get $mb) "\2a")"#;
    let out = run(
        "dylink_memory_base",
        &component("", &[lib], "", LIFT_PEEK),
        "println!(\"{} {}\", Exports::peek(&mut inst, 64), Exports::peek(&mut inst, 65));",
    );
    assert_eq!(out, "42 0");
}

/// (d) Two libraries with disjoint table bases (7 and 9) share the table.
#[test]
fn two_libraries_share_the_table_at_disjoint_bases() {
    let a = r#"(func $g (result i32) i32.const 71)
        (elem (global.get $tb) $g)"#;
    let b = r#"(func $g (result i32) i32.const 72)
        (elem (global.get $tb) $g)"#;
    let out = run(
        "dylink_two_libs",
        &component("", &[a, b], "", LIFT_CALL_AT),
        "println!(\"{} {}\", Exports::call_at(&mut inst, 7), Exports::call_at(&mut inst, 9));",
    );
    assert_eq!(out, "71 72");
}

/// (f) Module starts run in instantiation order: the library's, then `__init`'s.
#[test]
fn a_start_function_of_init_runs_after_the_librarys() {
    let lib = r#"(func $s (i32.store8 (i32.const 0) (i32.const 1))) (start $s)"#;
    let init = r#"(func $s
            (i32.store8 (i32.const 0)
                (i32.add (i32.mul (i32.load8_u (i32.const 0)) (i32.const 10)) (i32.const 2))))
        (start $s)"#;
    let out = run(
        "dylink_init_start",
        &component("", &[lib], init, LIFT_PEEK),
        "println!(\"{}\", Exports::peek(&mut inst, 0));",
    );
    assert_eq!(out, "12");
}

/// A component exporting `wasi:cli/run` from a library whose result says whether the table
/// entry its `__table_base` names is the expected function.
fn run_component(expected: u32) -> Vec<u8> {
    let lib = format!(
        r#"(type $t (func (result i32)))
        (func $g (result i32) i32.const 77)
        (elem (global.get $tb) $g)
        (func (export "run") (result i32)
            (i32.ne (call_indirect (type $t) (global.get $tb)) (i32.const {expected})))"#
    );
    let lifts = r#"
    (func $run (result (result)) (canon lift (core func $l0 "run")))
    (instance $r (export "run" (func $run)))
    (export "wasi:cli/run@0.2.0" (instance $r))"#;
    component("", &[&lib], "", lifts)
}

/// (g) `wasi:cli/run#run` of a dylink-shaped component returns its exit code.
#[test]
fn a_dylink_shaped_component_runs_wasi_cli_run() {
    let out = run(
        "dylink_cli_run",
        &run_component(77),
        "println!(\"{:?}\", Exports::export_wasi_cli_run_0_2_0_run(&mut inst));",
    );
    assert_eq!(out, "Ok(())");
    let out = run(
        "dylink_cli_run_failing",
        &run_component(5),
        "println!(\"{:?}\", Exports::export_wasi_cli_run_0_2_0_run(&mut inst));",
    );
    assert_eq!(out, "Err(())");
}

/// A library whose element offset is a plain constant is unaffected by the base folding.
#[test]
fn a_library_without_a_table_base_import_is_not_affected() {
    let lib = r#"(func $g (result i32) i32.const 3) (elem (i32.const 2) $g)"#;
    let out = run(
        "dylink_constant_elem",
        &component("", &[lib], "", LIFT_CALL_AT),
        "println!(\"{}\", Exports::call_at(&mut inst, 2));",
    );
    assert_eq!(out, "3");
}

/// A component that lifts only from `main`, with one library that shares its memory and table
/// and has `lib_body` as its items. The library exports nothing lifted.
fn main_only_component(lib_body: &str) -> Vec<u8> {
    let wat = format!(
        r#"(component
    (import "t:t/i" (instance $i (export "f" (func (result u32)))))
    (alias export $i "f" (func $hf))
    (core func $lowered (canon lower (func $hf)))
    (core instance $host (export "f" (func $lowered)))
    (core module $main
        (import "t:t/i" "f" (func (result i32)))
        (memory (export "memory") 1)
        (table (export "table") 16 funcref)
        (type $t (func (result i32)))
        (func (export "call_at") (param i32) (result i32) (call_indirect (type $t) (local.get 0)))
        (func (export "peek") (param i32) (result i32) (i32.load8_u (local.get 0))))
    (core module $lib
        (import "env" "memory" (memory 1))
        (import "env" "table" (table 16 funcref))
        {lib_body})
    (core instance $m (instantiate $main (with "t:t/i" (instance $host))))
    (alias core export $m "memory" (core memory $mem))
    (alias core export $m "table" (core table $tbl))
    (core instance $env (export "memory" (memory $mem)) (export "table" (table $tbl)))
    (core instance $l (instantiate $lib (with "env" (instance $env))))
    {LIFT_CALL_AT}
    {LIFT_PEEK})"#
    );
    wat::parse_str(wat).unwrap()
}

/// A library that is instantiated but not lifted from still fills the shared table.
#[test]
fn an_unlifted_library_still_fills_the_shared_table() {
    let lib = r#"(func $g (result i32) i32.const 77) (elem (i32.const 7) $g)"#;
    let out = run(
        "dylink_main_only_table",
        &main_only_component(lib),
        "println!(\"{}\", Exports::call_at(&mut inst, 7));",
    );
    assert_eq!(out, "77");
}

/// A library that is instantiated but not lifted from still runs its start function.
#[test]
fn an_unlifted_librarys_start_function_runs() {
    let lib = r#"(func $s (i32.store8 (i32.const 0) (i32.const 9))) (start $s)"#;
    let out = run(
        "dylink_main_only_start",
        &main_only_component(lib),
        "println!(\"{}\", Exports::peek(&mut inst, 0));",
    );
    assert_eq!(out, "9");
}
