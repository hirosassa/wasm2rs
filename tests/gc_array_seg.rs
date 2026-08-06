//! End-to-end tests for GC phase G3: building/initialising GC arrays from
//! passive data and element segments — `array.new_data`, `array.init_data`,
//! `array.new_elem`, `array.init_elem`.
//!
//! `*_data` reads little-endian numeric elements out of a passive `data`
//! segment; `*_elem` reads funcrefs out of a passive `elem` segment. Because
//! these read the retained (`self.data{d}`/`self.elem{e}`) passive segments,
//! using any of them forces the module to become a stateful `Instance`.

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
fn array_new_data_reads_i32_elements() {
    // `array.new_data` reads `size` little-endian i32s out of the data segment
    // starting at the byte offset, building a fresh array.
    compile_run(
        "gc_array_new_data_i32",
        r#"(module
            (type $arr (array (mut i32)))
            (data $d "\01\00\00\00\02\00\00\00\03\00\00\00")
            (func (export "at1") (result i32)
              i32.const 0 i32.const 3 array.new_data $arr $d
              i32.const 1 array.get $arr)
            (func (export "len") (result i32)
              i32.const 0 i32.const 3 array.new_data $arr $d array.len))"#,
        "let mut inst = Instance::new(); \
         assert_eq!(inst.func0(), 2); assert_eq!(inst.func1(), 3);",
    );
}

#[test]
fn array_new_data_reads_packed_i8_elements() {
    // A packed `i8` array reads one byte per element, zero-extended by
    // `array.get_u`.
    compile_run(
        "gc_array_new_data_i8",
        r#"(module
            (type $arr (array (mut i8)))
            (data $d "\0a\14\1e")
            (func (export "at2") (result i32)
              i32.const 0 i32.const 3 array.new_data $arr $d
              i32.const 2 array.get_u $arr))"#,
        "let mut inst = Instance::new(); assert_eq!(inst.func0(), 30);",
    );
}

#[test]
fn array_init_data_copies_into_an_existing_array() {
    // `array.init_data` copies `size` elements from the data segment (at a byte
    // offset) into the array starting at a destination index.
    compile_run(
        "gc_array_init_data",
        r#"(module
            (type $arr (array (mut i32)))
            (data $d "\63\00\00\00\64\00\00\00")
            (func (export "f") (result i32)
              (local $a (ref $arr))
              i32.const 0 i32.const 3 array.new $arr local.set $a
              local.get $a i32.const 1 i32.const 0 i32.const 2 array.init_data $arr $d
              local.get $a i32.const 2 array.get $arr))"#,
        "let mut inst = Instance::new(); assert_eq!(inst.func0(), 100);",
    );
}

#[test]
fn array_new_elem_reads_funcrefs() {
    // `array.new_elem` builds an array of funcrefs from an element segment; a read
    // element is dispatchable with `call_ref`.
    compile_run(
        "gc_array_new_elem",
        r#"(module
            (type $ft (func (param i32) (result i32)))
            (type $arr (array (ref $ft)))
            (func $add1 (param i32) (result i32) local.get 0 i32.const 1 i32.add)
            (func $dbl (param i32) (result i32) local.get 0 i32.const 2 i32.mul)
            (elem $e func $add1 $dbl)
            (func (export "run") (param i32) (result i32)
              (local $f (ref $ft))
              i32.const 0 i32.const 2 array.new_elem $arr $e
              i32.const 1 array.get $arr local.set $f
              local.get 0
              local.get $f
              call_ref $ft))"#,
        "let mut inst = Instance::new(); assert_eq!(inst.func2(21), 42);",
    );
}

#[test]
fn array_init_elem_copies_funcrefs_into_an_array() {
    // `array.init_elem` copies funcrefs from an element segment into an existing
    // array.
    compile_run(
        "gc_array_init_elem",
        r#"(module
            (type $ft (func (param i32) (result i32)))
            (type $arr (array (ref null $ft)))
            (func $add1 (param i32) (result i32) local.get 0 i32.const 1 i32.add)
            (func $dbl (param i32) (result i32) local.get 0 i32.const 2 i32.mul)
            (elem $e func $add1 $dbl)
            (func (export "run") (param i32) (result i32)
              (local $a (ref $arr))
              i32.const 2 array.new_default $arr local.set $a
              local.get $a i32.const 0 i32.const 1 i32.const 1 array.init_elem $arr $e
              local.get 0
              local.get $a i32.const 0 array.get $arr
              call_ref $ft))"#,
        "let mut inst = Instance::new(); assert_eq!(inst.func2(21), 42);",
    );
}
