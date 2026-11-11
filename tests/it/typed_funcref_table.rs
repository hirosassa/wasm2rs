//! A table whose element type is a concrete function reference
//! (`(table 3 (ref null $ft))`) is represented exactly like a `funcref` table.
//! A table of GC struct/array references stays rejected.

use crate::common;

#[test]
fn call_indirect_through_a_typed_funcref_table() {
    common::compile_run(
        "typed_table_call_indirect",
        r#"(module
             (type $ft (func (result i32)))
             (func $a (result i32) i32.const 11)
             (func $b (result i32) i32.const 22)
             (table $t 2 (ref null $ft))
             (elem (table $t) (i32.const 0) (ref null $ft) (ref.func $a) (ref.func $b))
             (func (export "f") (param i32) (result i32)
               local.get 0 call_indirect $t (type $ft)))"#,
        "let mut inst = Instance::new();\n assert_eq!(inst.func2(0), 11);\n assert_eq!(inst.func2(1), 22);",
    );
}

#[test]
fn table_get_set_and_call_ref_on_a_typed_funcref_table() {
    common::compile_run(
        "typed_table_get_set",
        r#"(module
             (type $ft (func (result i32)))
             (func $a (result i32) i32.const 5)
             (table $t 2 (ref null $ft))
             (elem declare func $a)
             (func (export "f") (result i32)
               i32.const 1 ref.func $a table.set $t
               i32.const 1 table.get $t
               ref.as_non_null call_ref $ft))"#,
        "let mut inst = Instance::new();\n assert_eq!(inst.func1(), 5);",
    );
}

#[test]
fn a_table_of_gc_struct_references_is_still_rejected() {
    let wasm = wat::parse_str(r#"(module (type $s (struct)) (table 1 (ref null $s)))"#).unwrap();
    let err = wasm2rs::transpile(&wasm).unwrap_err().to_string();
    assert!(
        err.contains("table of a non-funcref/externref type"),
        "{err}"
    );
}

#[test]
fn br_on_null_and_br_on_non_null_work_on_funcrefs() {
    common::compile_run(
        "funcref_br_on_null",
        r#"(module
             (type $ft (func (result i32)))
             (func $a (result i32) i32.const 5)
             (table $t 2 (ref null $ft))
             (elem (table $t) (i32.const 0) (ref null $ft) (ref.func $a))
             (func (export "on_null") (param i32) (result i32)
               (block $n
                 local.get 0 table.get $t
                 br_on_null $n
                 drop
                 i32.const 1 return)
               i32.const 0)
             (func (export "on_non_null") (param i32) (result i32)
               (block $nn (result (ref $ft))
                 local.get 0 table.get $t
                 br_on_non_null $nn
                 i32.const -1 return)
               call_ref $ft))"#,
        "let mut inst = Instance::new();\n \
         assert_eq!(inst.func1(0), 1);\n assert_eq!(inst.func1(1), 0);\n \
         assert_eq!(inst.func2(0), 5);\n assert_eq!(inst.func2(1), -1);",
    );
}
