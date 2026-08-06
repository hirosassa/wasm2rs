//! End-to-end tests for GC phase G2: `funcref`/`externref` stored in `struct`
//! and `array` fields.
//!
//! A managed object's slots are `GcSlot`s, which previously had no `u32` variant,
//! so a funcref/externref field (represented as a `u32`) could not be stored — a
//! `struct.new`/`struct.new_default` on such a field was rejected. Adding
//! `GcSlot::Func(u32)` lets these fields round-trip: a funcref read back out of a
//! field is still callable via `call_ref`, and a defaulted field is the null
//! (`u32::MAX`) funcref/externref.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::arithmetic_side_effects,
    clippy::float_cmp,
    clippy::lossy_float_literal,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::unwrap_in_result,
    reason = "test code"
)]

mod common;

use common::compile_run;

#[test]
fn funcref_struct_field_round_trips_and_calls() {
    // A `(ref $ft)` stored in a struct field is read back and dispatched with
    // `call_ref`, so the field preserved the function reference.
    compile_run(
        "gc_field_funcref_struct",
        r#"(module
            (type $ft (func (param i32) (result i32)))
            (type $s (struct (field (ref $ft))))
            (func $add1 (param i32) (result i32)
              local.get 0 i32.const 1 i32.add)
            (elem declare func $add1)
            (func (export "run") (param i32) (result i32)
              (local $f (ref $ft))
              ref.func $add1 struct.new $s
              struct.get $s 0 local.set $f
              local.get 0
              local.get $f
              call_ref $ft))"#,
        "let mut inst = Instance::new(); assert_eq!(inst.func1(41), 42);",
    );
}

#[test]
fn funcref_array_element_round_trips_and_calls() {
    // A funcref stored in an array element is read back and dispatched.
    compile_run(
        "gc_field_funcref_array",
        r#"(module
            (type $ft (func (param i32) (result i32)))
            (type $arr (array (ref $ft)))
            (func $dbl (param i32) (result i32)
              local.get 0 i32.const 2 i32.mul)
            (elem declare func $dbl)
            (func (export "run") (param i32) (result i32)
              (local $f (ref $ft))
              ref.func $dbl i32.const 3 array.new $arr
              i32.const 0 array.get $arr local.set $f
              local.get 0
              local.get $f
              call_ref $ft))"#,
        "let mut inst = Instance::new(); assert_eq!(inst.func1(21), 42);",
    );
}

#[test]
fn externref_struct_field_stores_null() {
    // A null externref survives a round trip through a struct field: the field
    // reads back as null.
    compile_run(
        "gc_field_externref",
        r#"(module
            (type $s (struct (field externref)))
            (func (export "isnull") (result i32)
              ref.null extern struct.new $s
              struct.get $s 0 ref.is_null))"#,
        "assert_eq!(func0(), 1);",
    );
}

#[test]
fn struct_new_default_funcref_field_is_null() {
    // `struct.new_default` on a nullable funcref field yields the null funcref,
    // which `ref.is_null` reports (previously this was rejected).
    compile_run(
        "gc_field_funcref_default",
        r#"(module
            (type $ft (func (param i32) (result i32)))
            (type $s (struct (field (ref null $ft))))
            (func (export "isnull") (result i32)
              struct.new_default $s struct.get $s 0 ref.is_null))"#,
        "assert_eq!(func0(), 1);",
    );
}

#[test]
fn array_new_default_funcref_elements_are_null() {
    // `array.new_default` fills a nullable funcref array with the null funcref.
    compile_run(
        "gc_field_funcref_array_default",
        r#"(module
            (type $ft (func (param i32) (result i32)))
            (type $arr (array (ref null $ft)))
            (func (export "isnull") (result i32)
              i32.const 3 array.new_default $arr
              i32.const 1 array.get $arr ref.is_null))"#,
        "assert_eq!(func0(), 1);",
    );
}
