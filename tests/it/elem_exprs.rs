//! Element segments whose items are `ref.null`, and tables with an initializer
//! expression (`(table 2 funcref (ref.func $f))`).

use crate::common;

#[test]
fn a_null_item_in_an_active_segment_leaves_the_slot_null() {
    common::compile_run(
        "elem_null_item",
        r#"(module
             (table 3 funcref)
             (func $f (result i32) i32.const 7)
             (elem (i32.const 0) funcref (ref.func $f) (ref.null func) (ref.func $f))
             (func (export "is_null") (param i32) (result i32)
               (ref.is_null (table.get (local.get 0)))))"#,
        "let mut inst = Instance::new();\n\
         assert_eq!(inst.func1(0), 0);\n\
         assert_eq!(inst.func1(1), 1);\n\
         assert_eq!(inst.func1(2), 0);",
    );
}

#[test]
fn table_init_copies_null_items_from_a_passive_segment() {
    common::compile_run(
        "elem_null_table_init",
        r#"(module
             (table 2 funcref)
             (func $f)
             (elem funcref (ref.func $f) (ref.null func))
             (func (export "run") (result i32)
               (table.init 0 (i32.const 0) (i32.const 0) (i32.const 2))
               (i32.add
                 (ref.is_null (table.get (i32.const 0)))
                 (i32.shl (ref.is_null (table.get (i32.const 1))) (i32.const 1)))))"#,
        "let mut inst = Instance::new();\n\
         assert_eq!(inst.func1(), 2);",
    );
}

#[test]
fn calling_a_null_item_traps() {
    common::expect_trap(
        "elem_null_call",
        r#"(module
             (type $v (func))
             (table 2 funcref)
             (func $f)
             (elem (i32.const 0) funcref (ref.func $f) (ref.null func))
             (func (export "f") (call_indirect (type $v) (i32.const 1))))"#,
        "let mut inst = Instance::new();\n inst.func1();",
    );
}

#[test]
fn a_null_item_in_an_externref_segment_is_null() {
    common::compile_run(
        "elem_null_externref",
        r#"(module
             (table 2 externref)
             (elem (i32.const 0) externref (ref.null extern) (ref.null extern))
             (func (export "f") (result i32) (ref.is_null (table.get (i32.const 1)))))"#,
        "let mut inst = Instance::new();\n assert_eq!(inst.func0(), 1);",
    );
}

#[test]
fn a_ref_func_table_initializer_fills_every_entry() {
    common::compile_run(
        "table_init_ref_func",
        r#"(module
             (type $r (func (result i32)))
             (table 3 funcref (ref.func $f))
             (func $f (result i32) i32.const 9)
             (func (export "call") (param i32) (result i32)
               (call_indirect (type $r) (local.get 0))))"#,
        "let mut inst = Instance::new();\n\
         assert_eq!(inst.func1(0), 9);\n\
         assert_eq!(inst.func1(2), 9);",
    );
}

#[test]
fn a_ref_null_table_initializer_leaves_entries_null() {
    common::compile_run(
        "table_init_ref_null",
        r#"(module
             (table 2 funcref (ref.null func))
             (func (export "f") (result i32) (ref.is_null (table.get (i32.const 1)))))"#,
        "let mut inst = Instance::new();\n assert_eq!(inst.func0(), 1);",
    );
}

#[test]
fn a_table_initializer_does_not_apply_to_grown_entries() {
    // `table.grow` takes its own init value; the declared initializer only seeds
    // the initial entries.
    common::compile_run(
        "table_init_grow",
        r#"(module
             (table 1 funcref (ref.func $f))
             (func $f)
             (func (export "grow") (result i32)
               (drop (table.grow (ref.null func) (i32.const 1)))
               (ref.is_null (table.get (i32.const 1)))))"#,
        "let mut inst = Instance::new();\n assert_eq!(inst.func1(), 1);",
    );
}

#[test]
fn a_ref_func_initializer_keeps_its_function_alive() {
    // `$f` is reachable only through the table initializer.
    common::compile_run(
        "table_init_reach",
        r#"(module
             (type $r (func (result i32)))
             (table 1 funcref (ref.func $f))
             (func $f (result i32) i32.const 5)
             (func (export "call") (result i32) (call_indirect (type $r) (i32.const 0))))"#,
        "let mut inst = Instance::new();\n assert_eq!(inst.func1(), 5);",
    );
}

#[test]
fn an_element_item_may_read_a_defined_funcref_global() {
    common::compile_run(
        "elem_global_get",
        r#"(module
             (type $r (func (result i32)))
             (table 2 funcref)
             (func $f (result i32) i32.const 6)
             (global $g funcref (ref.func $f))
             (elem (i32.const 1) funcref (global.get $g))
             (func (export "call") (param i32) (result i32)
               (call_indirect (type $r) (local.get 0))))"#,
        "let mut inst = Instance::new();\n assert_eq!(inst.func1(1), 6);",
    );
}

#[test]
fn a_table_initializer_may_read_a_defined_funcref_global() {
    common::compile_run(
        "table_init_global_get",
        r#"(module
             (type $r (func (result i32)))
             (global $g funcref (ref.func $f))
             (table 2 funcref (global.get $g))
             (func $f (result i32) i32.const 8)
             (func (export "call") (param i32) (result i32)
               (call_indirect (type $r) (local.get 0))))"#,
        "let mut inst = Instance::new();\n assert_eq!(inst.func1(1), 8);",
    );
}

#[test]
fn a_null_funcref_global_gives_a_null_element_item() {
    common::compile_run(
        "elem_global_get_null",
        r#"(module
             (table 2 funcref)
             (global $g funcref (ref.null func))
             (elem (i32.const 0) funcref (global.get $g))
             (func (export "f") (result i32) (ref.is_null (table.get (i32.const 0)))))"#,
        "let mut inst = Instance::new();\n assert_eq!(inst.func0(), 1);",
    );
}

#[test]
fn a_global_initializer_may_read_an_earlier_immutable_global() {
    common::compile_run(
        "global_init_global_get",
        r#"(module
             (global $a i32 (i32.const 40))
             (global $b i32 (i32.add (global.get $a) (i32.const 2)))
             (func (export "f") (result i32) (global.get $b)))"#,
        "let mut inst = Instance::new();\n assert_eq!(inst.func0(), 42);",
    );
}
