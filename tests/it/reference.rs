//! Integration tests for Phase 9 (reference types): `ref.null func`,
//! `ref.func` and `ref.is_null`. A `funcref` value is represented as a `u32`
//! function index on the operand stack (`u32::MAX` is a null funcref), matching
//! the table's element representation. Modules here declare no memory/table/
//! global/import, so they transpile to free `func{n}` functions; the generated
//! Rust is compiled with `rustc -D warnings` and exercised.

use crate::common::compile_run;

#[test]
fn ref_is_null_distinguishes_null_from_a_function() {
    // `ref.is_null (ref.null func)` is 1; `ref.is_null (ref.func $target)` is 0.
    let wat = r#"
        (module
          (func $target (result i32) (i32.const 42))
          (elem declare func $target)
          (func (result i32) (ref.is_null (ref.null func)))
          (func (result i32) (ref.is_null (ref.func $target))))
        "#;
    compile_run(
        "ref_is_null",
        wat,
        "assert_eq!(func1(), 1);\n    assert_eq!(func2(), 0);",
    );
}

#[test]
fn funcref_round_trips_through_a_local() {
    // A `funcref` local is a `u32`; storing `ref.func` and reading it back keeps
    // it non-null.
    let wat = r#"
        (module
          (func $target (result i32) (i32.const 42))
          (elem declare func $target)
          (func (result i32) (local funcref)
            (local.set 0 (ref.func $target))
            (ref.is_null (local.get 0))))
        "#;
    compile_run("funcref_local", wat, "assert_eq!(func1(), 0);");
}

#[test]
fn funcref_passes_through_params_and_results() {
    // A function taking and returning a `funcref` is the identity on the `u32`;
    // passing a null through it stays null.
    let wat = r#"
        (module
          (func $id (param funcref) (result funcref) (local.get 0))
          (func (result i32) (ref.is_null (call $id (ref.null func)))))
        "#;
    compile_run("funcref_param", wat, "assert_eq!(func1(), 1);");
}

#[test]
fn declared_element_segment_allows_ref_func() {
    // A declared element segment has no runtime effect; it only permits
    // `ref.func`, so the module must still transpile and run.
    let wat = r#"
        (module
          (func $target (result i32) (i32.const 7))
          (elem declare func $target)
          (func (result i32) (ref.is_null (ref.func $target))))
        "#;
    compile_run("declared_elem", wat, "assert_eq!(func1(), 0);");
}

#[test]
fn declared_before_passive_keeps_table_init_index_aligned() {
    // A declared segment sits at element index 0 and a passive one at index 1.
    // `table.init 1` must reference the passive segment, so the declared slot
    // must not shift the passive segment's index.
    let wat = r#"
        (module
          (type $sig (func (result i32)))
          (table 2 funcref)
          (func $a (type $sig) (i32.const 55))
          (elem declare func $a)
          (elem func $a)
          (func (param i32 i32 i32) (table.init 1 (local.get 0) (local.get 1) (local.get 2)))
          (func (param i32) (result i32) (call_indirect (type $sig) (local.get 0))))
        "#;
    compile_run(
        "declared_before_passive",
        wat,
        "let mut inst = Instance::new();\n    \
         inst.func1(0, 0, 1);\n    \
         assert_eq!(inst.func2(0), 55);",
    );
}
