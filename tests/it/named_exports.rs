//! With `TranspileOptions::named_exports`, every exported (defined) function also
//! gets a method / free function named after its export, forwarding to `func{N}`.

use std::process::Command;

use crate::common;
use wasm2rs::{TranspileOptions, transpile, transpile_with_options};

fn named() -> TranspileOptions {
    TranspileOptions {
        named_exports: true,
        ..TranspileOptions::default()
    }
}

/// Transpile `wat` with `named_exports`, append `main_body`, run it, assert exit 0.
fn run_named(name: &str, wat: &str, main_body: &str) {
    let wasm = wat::parse_str(wat).unwrap();
    let generated = transpile_with_options(&wasm, &named()).unwrap();
    let program = format!("{generated}\nfn main() {{\n{main_body}\n}}\n");
    let bin = common::build(name, &program);
    let status = Command::new(&bin).status().unwrap();
    assert!(status.success(), "program assertions failed: {name}");
}

#[test]
fn stateful_export_is_callable_by_name() {
    run_named(
        "named_stateful",
        r#"(module
            (memory 1)
            (func (export "store_add") (param i32 i32)
              i32.const 0 local.get 0 local.get 1 i32.add i32.store)
            (func (export "load") (result i32) i32.const 0 i32.load))"#,
        "let mut inst = Instance::new();\n inst.store_add(40, 2);\n \
         assert_eq!(inst.load(), 42);\n assert_eq!(inst.func1(), 42);",
    );
}

#[test]
fn stateless_export_is_a_free_function() {
    run_named(
        "named_stateless",
        r#"(module (func (export "add") (param i32 i32) (result i32)
            local.get 0 local.get 1 i32.add))"#,
        "assert_eq!(add(1, 2), 3);\n assert_eq!(func0(1, 2), 3);",
    );
}

#[test]
fn all_value_types_and_multi_value_results() {
    run_named(
        "named_types",
        r#"(module (func (export "mix") (param i64 f32 f64) (result i32 i64)
            i32.const 7 local.get 0))"#,
        "assert_eq!(mix(5, 1.0, 2.0), (7, 5));",
    );
}

#[test]
fn names_are_made_valid_and_unique() {
    // `type` is a keyword, `my-func` has a hyphen, `1st` starts with a digit, and
    // `dup`/`dup` collide after sanitising (`a.b` and `a_b` both become `a_b`).
    run_named(
        "named_sanitised",
        r#"(module
            (func (export "type") (result i32) i32.const 1)
            (func (export "my-func") (result i32) i32.const 2)
            (func (export "1st") (result i32) i32.const 3)
            (func (export "a.b") (result i32) i32.const 4)
            (func (export "a_b") (result i32) i32.const 5))"#,
        "assert_eq!(type_(), 1);\n assert_eq!(my_func(), 2);\n assert_eq!(_1st(), 3);\n \
         assert_eq!(a_b(), 4);\n assert_eq!(a_b_4(), 5);",
    );
}

#[test]
fn names_clashing_with_generated_members_get_a_suffix() {
    // `new`, `memory`, `table` and `func0` are already taken on `Instance`.
    run_named(
        "named_clash",
        r#"(module
            (memory 1) (table 1 funcref)
            (func (export "new") (result i32) i32.const 1)
            (func (export "memory") (result i32) i32.const 2)
            (func (export "table") (result i32) i32.const 3)
            (func (export "func0") (result i32) i32.const 4))"#,
        "let mut inst = Instance::new();\n assert_eq!(inst.new_(), 1);\n \
         assert_eq!(inst.memory_(), 2);\n assert_eq!(inst.table_(), 3);\n \
         assert_eq!(inst.func0_(), 4);",
    );
}

#[test]
fn an_exported_wasi_thread_start_keeps_its_single_wrapper() {
    run_named(
        "named_wasi_thread_start",
        r#"(module (memory 1)
            (func (export "wasi_thread_start") (param i32 i32)
              i32.const 0 local.get 0 local.get 1 i32.add i32.store)
            (func (export "get") (result i32) i32.const 0 i32.load))"#,
        "let mut inst = Instance::new();\n inst.wasi_thread_start(40, 2);\n \
         assert_eq!(inst.get(), 42);",
    );
}

#[test]
fn off_by_default_the_output_is_unchanged() {
    let wasm = wat::parse_str(
        r#"(module (func (export "add") (param i32 i32) (result i32)
            local.get 0 local.get 1 i32.add))"#,
    )
    .unwrap();
    let plain = transpile(&wasm).unwrap();
    let with_default = transpile_with_options(&wasm, &TranspileOptions::default()).unwrap();
    assert_eq!(plain, with_default);
    assert!(!plain.contains("pub fn add("), "{plain}");
}

#[test]
fn exported_globals_get_a_getter_that_tracks_the_current_value() {
    run_named(
        "named_global_getter",
        r#"(module
            (global $sp (export "stack_pointer") (mut i32) (i32.const 1024))
            (global (export "big") i64 (i64.const 7))
            (global (export "ratio") (mut f64) (f64.const 0.5))
            (func (export "bump") (global.set $sp (i32.add (global.get $sp) (i32.const 16)))))"#,
        "let mut inst = Instance::new();\n \
         assert_eq!(inst.stack_pointer(), 1024);\n \
         assert_eq!(inst.big(), 7);\n \
         assert_eq!(inst.ratio(), 0.5);\n \
         inst.bump();\n \
         assert_eq!(inst.stack_pointer(), 1040);",
    );
}

#[test]
fn a_global_name_clashing_with_a_function_export_stays_unique() {
    run_named(
        "named_global_clash",
        r#"(module
            (global (export "x") i32 (i32.const 5))
            (func (export "x") (result i32) i32.const 9))"#,
        "let mut inst = Instance::new();\n \
         assert_eq!(inst.x(), 9);\n \
         assert_eq!(inst.x_0(), 5);",
    );
}

#[test]
fn global_getters_are_off_without_named_exports() {
    let wasm =
        wat::parse_str(r#"(module (global (export "sp") (mut i32) (i32.const 1)))"#).unwrap();
    let generated = transpile(&wasm).unwrap();
    assert!(!generated.contains("pub fn sp("), "{generated}");
}

#[test]
fn an_exported_imported_global_reads_through_the_host_and_non_numeric_ones_are_skipped() {
    let wasm = wat::parse_str(
        r#"(module
            (import "env" "g" (global $g i32))
            (global (export "again") i32 (global.get $g))
            (export "reexport" (global $g))
            (global (export "fn_ref") (mut funcref) (ref.null func))
            (global (export "wide") v128 (v128.const i64x2 1 2)))"#,
    )
    .unwrap();
    let generated = transpile_with_options(&wasm, &named()).unwrap();
    assert!(
        generated.contains("pub fn reexport(&self) -> i32 { self.imports.get_global0() }"),
        "{generated}"
    );
    assert!(
        generated.contains("pub fn again(&self) -> i32 { self.g1 }"),
        "{generated}"
    );
    assert!(
        !generated.contains("fn fn_ref(") && !generated.contains("fn wide("),
        "{generated}"
    );
}

#[test]
fn a_global_named_like_a_memory_accessor_stays_clear_of_it() {
    run_named(
        "named_global_memory1",
        r#"(module
            (memory 1) (memory 1)
            (global (export "memory1") i32 (i32.const 8)))"#,
        "let mut inst = Instance::new();\n \
         assert_eq!(inst.memory1_(), 8);\n \
         assert_eq!(inst.memory1().len(), 65536);",
    );
}

#[test]
fn a_global_named_shared_memory_stays_clear_of_the_shared_accessor() {
    run_named(
        "named_global_shared_memory",
        r#"(module
            (memory 1 1 shared)
            (global (export "shared_memory") i32 (i32.const 8)))"#,
        "let inst = Instance::new();\n \
         assert_eq!(inst.shared_memory_(), 8);",
    );
}

#[test]
fn a_global_named_like_a_split_dispatch_part_stays_clear_of_it() {
    // `func0_part0` is a generated sibling function name, not just a member.
    let wasm =
        wat::parse_str(r#"(module (global (export "func0_part0") i32 (i32.const 1)))"#).unwrap();
    let generated = transpile_with_options(&wasm, &named()).unwrap();
    assert!(
        generated.contains("pub fn func0_part0_(&self)"),
        "{generated}"
    );
}
