//! The bottom reference types `nullfuncref`, `nullexternref` and `nullexnref` hold
//! only null, so they lower like their top types: a `u32` handle, `u32::MAX` null.

use crate::common;

#[test]
fn ref_null_of_a_bottom_type_is_null() {
    common::compile_run(
        "bottom_ref_null",
        r#"(module
          (func (export "nofunc") (result i32) (ref.is_null (ref.null nofunc)))
          (func (export "noextern") (result i32) (ref.is_null (ref.null noextern)))
          (func (export "noexn") (result i32) (ref.is_null (ref.null noexn))))"#,
        "assert_eq!(func0(), 1);\n assert_eq!(func1(), 1);\n assert_eq!(func2(), 1);",
    );
}

#[test]
fn a_bottom_type_flows_through_params_locals_results_and_globals() {
    common::compile_run(
        "bottom_ref_flow",
        r#"(module
          (global $g nullfuncref (ref.null nofunc))
          (func $id (param nullfuncref) (result nullfuncref)
            (local $l nullexternref)
            (local.get 0))
          (func (export "is_null") (result i32)
            (ref.is_null (call $id (global.get $g))))
          (func (export "as_funcref") (result funcref) (global.get $g)))"#,
        "let mut inst = Instance::new();\n assert_eq!(inst.func1(), 1);\n \
         assert_eq!(inst.func2(), u32::MAX);",
    );
}

#[test]
fn a_table_of_a_bottom_type_is_all_null() {
    common::compile_run(
        "bottom_ref_table",
        r#"(module
          (table 3 (ref null nofunc))
          (func (export "size") (result i32) (table.size 0))
          (func (export "is_null") (param i32) (result i32)
            (ref.is_null (table.get 0 (local.get 0)))))"#,
        "let mut inst = Instance::new();\n assert_eq!(inst.func0(), 3);\n \
         assert_eq!(inst.func1(2), 1);",
    );
}

#[test]
fn an_anyref_that_only_appears_in_types_still_gets_the_gc_value_model() {
    // No body creates a GC reference: `anyref` shows up in a global's type, a
    // parameter and a result only.
    common::compile_run(
        "gc_ref_in_types_only",
        r#"(module
          (global $g anyref (ref.null any))
          (func $id (param anyref) (result anyref) (local.get 0))
          (func (export "is_null") (result i32)
            (ref.is_null (call $id (global.get $g)))))"#,
        "let mut inst = Instance::new();\n assert_eq!(inst.func1(), 1);",
    );
}

#[test]
fn a_nullref_global_works_without_any_gc_type_or_gc_operator() {
    common::compile_run(
        "nullref_global",
        r#"(module
          (global $n nullref (ref.null none))
          (func (export "get") (result nullref) (global.get $n))
          (func (export "is_null") (result i32) (ref.is_null (global.get $n))))"#,
        "let mut inst = Instance::new();\n let _ = inst.func0();\n assert_eq!(inst.func1(), 1);",
    );
}

#[test]
fn an_anyref_only_inside_a_called_function_type_still_gets_the_gc_value_model() {
    // `anyref` appears only in the function type behind `call_ref`'s `(ref null $ft)`,
    // which itself lowers to a `u32`.
    common::compile_run(
        "gc_ref_in_call_ref_type",
        r#"(module
          (type $ft (func (result anyref)))
          (func (export "x") (param (ref null $ft)) (result i32)
            (ref.is_null (call_ref $ft (local.get 0)))))"#,
        "let mut inst = Instance::new();\n let _ = Instance::func0 as fn(&mut Instance, u32) -> i32;\n let _ = &mut inst;",
    );
}

#[test]
fn an_anyref_only_as_a_block_result_still_gets_the_gc_value_model() {
    common::compile_run(
        "gc_ref_in_block_type",
        r#"(module
          (func (export "x") (result i32)
            (block (result anyref) (unreachable)) (ref.is_null))
          (func (export "y") (param i32) (result i32)
            (if (result anyref) (local.get 0)
              (then (unreachable)) (else (unreachable)))
            (ref.is_null)))"#,
        "let _ = func0 as fn() -> i32;\n let _ = func1 as fn(i32) -> i32;",
    );
}
