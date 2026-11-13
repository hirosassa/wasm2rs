//! Lifts from several core modules whose core export names clash: the linked unit names what
//! it exports after the lifts' WIT names, and keeps no internal wiring export.

use std::process::Command;

use wasm2rs::{ComponentOptions, transpile_component};

use crate::common;

fn own_host() -> ComponentOptions {
    ComponentOptions {
        external_types: None,
        ..ComponentOptions::default()
    }
}

fn run(name: &str, code: &str, main: &str) -> String {
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

/// Both modules export `run`, and the program also exports an unlifted `run`-named helper of
/// the other module's lift: the exports are told apart by the lifts' WIT names.
#[test]
fn lifts_whose_core_export_names_clash_both_run() {
    let component = wat::parse_str(
        r#"(component
            (import "t:t/i" (instance $i (export "f" (func (result u32)))))
            (alias export $i "f" (func $hf))
            (core func $lowered (canon lower (func $hf)))
            (core instance $host (export "f" (func $lowered)))
            (core module $a
                (import "t:t/i" "f" (func $f (result i32)))
                (memory (export "memory") 1)
                (func (export "run") (result i32) (i32.add (call $f) (i32.const 1))))
            (core module $b (import "env" "memory" (memory 1))
                (func (export "run") (result i32) i32.const 2)
                (func (export "fa") (result i32) i32.const 9))
            (core instance $ia (instantiate $a (with "t:t/i" (instance $host))))
            (alias core export $ia "memory" (core memory $mem))
            (core instance $env (export "memory" (memory $mem)))
            (core instance $ib (instantiate $b (with "env" (instance $env))))
            (func $fa (result u32) (canon lift (core func $ia "run")))
            (func $fb (result u32) (canon lift (core func $ib "run")))
            (export "fa" (func $fa))
            (export "fb" (func $fb)))"#,
    )
    .unwrap();
    let code = transpile_component(&component, &own_host()).unwrap();
    let out = run(
        "component_link_clashing_names",
        &code,
        "println!(\"{} {}\", Exports::fa(&mut inst), Exports::fb(&mut inst));",
    );
    assert_eq!(out, "1 2");
}

/// The program's `_start` is wiring for the adapter module, not an export of the unit.
#[test]
fn internal_wiring_exports_are_not_exported() {
    let component = wat::parse_str(
        r#"(component
            (import "t:t/i" (instance $i (export "f" (func (result u32)))))
            (alias export $i "f" (func $hf))
            (core func $lowered (canon lower (func $hf)))
            (core instance $host (export "f" (func $lowered)))
            (core module $main
                (memory (export "memory") 1)
                (func (export "_start") (i32.store (i32.const 8) (i32.const 41))))
            (core module $adapter
                (import "t:t/i" "f" (func $f (result i32)))
                (import "env" "memory" (memory 1))
                (import "main" "_start" (func $start))
                (func (export "run") (result i32)
                    call $start
                    (i32.add (i32.load (i32.const 8)) (i32.add (call $f) (i32.const 1)))))
            (core instance $m (instantiate $main (with "t:t/i" (instance $host))))
            (alias core export $m "memory" (core memory $mem))
            (alias core export $m "_start" (core func $start))
            (core instance $env (export "memory" (memory $mem)))
            (core instance $mainfns (export "_start" (func $start)))
            (core instance $a (instantiate $adapter
                (with "env" (instance $env))
                (with "t:t/i" (instance $host))
                (with "main" (instance $mainfns))))
            (alias core export $a "run" (core func $run))
            (func (export "run") (result u32) (canon lift (core func $run))))"#,
    )
    .unwrap();
    let code = transpile_component(&component, &own_host()).unwrap();
    assert!(!code.contains("fn _start"), "{code}");
    let out = run(
        "component_link_no_internal_exports",
        &code,
        "println!(\"{}\", Exports::run(&mut inst));",
    );
    assert_eq!(out, "42");
}

/// Each module has its own `post-return` under the same core name: they stay separate.
#[test]
fn post_returns_with_the_same_core_name_stay_separate() {
    let component = wat::parse_str(
        r#"(component
            (import "t:t/i" (instance $i (export "f" (func (result u32)))))
            (alias export $i "f" (func $hf))
            (core func $lowered (canon lower (func $hf)))
            (core instance $host (export "f" (func $lowered)))
            (core module $a
                (import "t:t/i" "f" (func $h (result i32)))
                (memory (export "memory") 1)
                (func (export "f") (result i32) (i32.add (call $h) (i32.load (i32.const 0))))
                (func (export "post") (param i32) (i32.store (i32.const 0) (i32.const 100))))
            (core module $b (import "env" "memory" (memory 1))
                (func (export "f") (result i32) (i32.load (i32.const 4)))
                (func (export "post") (param i32) (i32.store (i32.const 4) (i32.const 200))))
            (core instance $ia (instantiate $a (with "t:t/i" (instance $host))))
            (alias core export $ia "memory" (core memory $mem))
            (core instance $env (export "memory" (memory $mem)))
            (core instance $ib (instantiate $b (with "env" (instance $env))))
            (func $fa (result u32) (canon lift (core func $ia "f") (post-return (core func $ia "post"))))
            (func $fb (result u32) (canon lift (core func $ib "f") (post-return (core func $ib "post"))))
            (export "fa" (func $fa))
            (export "fb" (func $fb)))"#,
    )
    .unwrap();
    let code = transpile_component(&component, &own_host()).unwrap();
    let out = run(
        "component_link_post_returns",
        &code,
        "let a1 = Exports::fa(&mut inst); let a2 = Exports::fa(&mut inst);\n\
         let b1 = Exports::fb(&mut inst); let b2 = Exports::fb(&mut inst);\n\
         println!(\"{a1} {a2} {b1} {b2}\");",
    );
    assert_eq!(out, "0 100 0 200");
}

/// Contract: lifts that allocate through different `realloc` functions are not supported yet;
/// the message says which module's realloc is the odd one out.
#[test]
fn allocating_lifts_with_different_reallocs_are_refused() {
    let component = wat::parse_str(
        r#"(component
            (core module $a (memory (export "memory") 1)
                (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32) i32.const 64)
                (func (export "fa") (param i32 i32) (result i32) i32.const 3))
            (core module $b (import "env" "memory" (memory $m 1)) (export "memory" (memory $m))
                (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32) i32.const 512)
                (func (export "fb") (param i32 i32) (result i32) i32.const 4))
            (core instance $ia (instantiate $a))
            (alias core export $ia "memory" (core memory $mem))
            (alias core export $ia "cabi_realloc" (core func $ra))
            (core instance $env (export "memory" (memory $mem)))
            (core instance $ib (instantiate $b (with "env" (instance $env))))
            (alias core export $ib "cabi_realloc" (core func $rb))
            (alias core export $ib "memory" (core memory $memb))
            (func $fa (param "s" string) (result u32)
                (canon lift (core func $ia "fa") (memory $mem) (realloc (core func $ra))))
            (func $fb (param "s" string) (result u32)
                (canon lift (core func $ib "fb") (memory $memb) (realloc (core func $rb))))
            (export "fa" (func $fa))
            (export "fb" (func $fb)))"#,
    )
    .unwrap();
    let err = transpile_component(&component, &own_host())
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("fb") && err.contains("core module 1") && err.contains("different `realloc`"),
        "{err}"
    );
}

/// Two reallocs of one appended module, neither the first lift's, behind lifts that do not
/// allocate: each keeps its own export name instead of clashing.
#[test]
fn several_unused_reallocs_of_one_module_do_not_clash() {
    let component = wat::parse_str(
        r#"(component
            (core module $a (memory (export "memory") 1)
                (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32) i32.const 64)
                (func (export "fa") (result i32) i32.const 1))
            (core module $b (import "env" "memory" (memory $m 1)) (export "memory" (memory $m))
                (func (export "r1") (param i32 i32 i32 i32) (result i32) i32.const 1)
                (func (export "r2") (param i32 i32 i32 i32) (result i32) i32.const 2)
                (func (export "fb") (result i32) i32.const 2)
                (func (export "fc") (result i32) i32.const 3))
            (core instance $ia (instantiate $a))
            (alias core export $ia "memory" (core memory $mem))
            (alias core export $ia "cabi_realloc" (core func $ra))
            (core instance $env (export "memory" (memory $mem)))
            (core instance $ib (instantiate $b (with "env" (instance $env))))
            (alias core export $ib "r1" (core func $r1))
            (alias core export $ib "r2" (core func $r2))
            (alias core export $ib "memory" (core memory $memb))
            (func $fa (result u32) (canon lift (core func $ia "fa") (memory $mem) (realloc (core func $ra))))
            (func $fb (result u32) (canon lift (core func $ib "fb") (memory $memb) (realloc (core func $r1))))
            (func $fc (result u32) (canon lift (core func $ib "fc") (memory $memb) (realloc (core func $r2))))
            (export "fa" (func $fa))
            (export "fb" (func $fb))
            (export "fc" (func $fc)))"#,
    )
    .unwrap();
    transpile_component(&component, &own_host()).unwrap();
}

const DYLINK_MEMORY: &str = r#"(component
    (import "t:t/i" (instance $i (export "f" (func (result u32)))))
    (alias export $i "f" (func $hf))
    (core func $lowered (canon lower (func $hf)))
    (core instance $host (export "f" (func $lowered)))
    (core module $main
        (import "t:t/i" "f" (func (result i32)))
        (memory (export "memory") 1)
        (data (i32.const 32) "hi"))
    (core module $other (memory (export "memory") 1))
    (core module $lib
        (import "env" "memory" (memory 1))
        (func (export "f") (result i32)
            (i32.store (i32.const 16) (i32.const 32))
            (i32.store (i32.const 20) (i32.const 2))
            (i32.const 16)))
    (core instance $m (instantiate $main (with "t:t/i" (instance $host))))
    (alias core export $m "memory" (core memory $mem))
    UNRELATED
    (core instance $env (export "memory" (memory $mem)))
    (core instance $l (instantiate $lib (with "env" (instance $env))))
    (func (export "f") (result string) (canon lift (core func $l "f") (memory MEMORY))))"#;

/// A library imports the program's memory, and its lift names that memory (an export of the
/// program, not of the library): the option resolves through the link.
#[test]
fn lift_memory_option_resolves_through_the_link() {
    let component = wat::parse_str(
        DYLINK_MEMORY
            .replace("MEMORY", "$mem")
            .replace("UNRELATED", ""),
    )
    .unwrap();
    let code = transpile_component(&component, &own_host()).unwrap();
    let out = run(
        "component_link_memory_option",
        &code,
        "println!(\"{}\", Exports::f(&mut inst));",
    );
    assert_eq!(out, "hi");
}

/// A memory option naming a memory the function's module does not use stays rejected.
#[test]
fn lift_memory_option_of_an_unrelated_memory_is_rejected() {
    let component = wat::parse_str(DYLINK_MEMORY.replace("MEMORY", "$unrelated").replace(
        "UNRELATED",
        r#"(core instance $o (instantiate $other))
                (alias core export $o "memory" (core memory $unrelated))"#,
    ))
    .unwrap();
    let err = transpile_component(&component, &own_host()).unwrap_err();
    assert!(err.to_string().contains("its memory is neither"), "{err}");
}
