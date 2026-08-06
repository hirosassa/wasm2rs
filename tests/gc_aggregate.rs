//! End-to-end tests for GC phase 4c-3: the remaining aggregate constructors and
//! bulk array operators — `struct.new_default`, `array.new_default`,
//! `array.new_fixed`, `array.fill`, and `array.copy`.

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
fn struct_new_default_zeroes_fields() {
    // `struct.new_default` fills every field with its default (0 for numerics).
    compile_run(
        "gc_struct_new_default",
        r#"(module
            (type $s (struct (field i32) (field i64)))
            (func (export "a") (result i32)
              struct.new_default $s struct.get $s 0)
            (func (export "b") (result i64)
              struct.new_default $s struct.get $s 1))"#,
        "assert_eq!(func0(), 0); assert_eq!(func1(), 0);",
    );
}

#[test]
fn array_new_default_zeroes_elements() {
    // `array.new_default` builds `size` default (0) elements.
    compile_run(
        "gc_array_new_default",
        r#"(module
            (type $arr (array (mut i32)))
            (func (export "get") (result i32)
              i32.const 4 array.new_default $arr i32.const 2 array.get $arr)
            (func (export "len") (result i32)
              i32.const 4 array.new_default $arr array.len))"#,
        "assert_eq!(func0(), 0); assert_eq!(func1(), 4);",
    );
}

#[test]
fn array_new_fixed_takes_elements_from_the_stack() {
    // `array.new_fixed $t N` pops N elements (last on top) into a fresh array.
    compile_run(
        "gc_array_new_fixed",
        r#"(module
            (type $arr (array (mut i32)))
            (func (export "mid") (result i32)
              i32.const 10 i32.const 20 i32.const 30 array.new_fixed $arr 3
              i32.const 1 array.get $arr)
            (func (export "len") (result i32)
              i32.const 10 i32.const 20 i32.const 30 array.new_fixed $arr 3 array.len))"#,
        "assert_eq!(func0(), 20); assert_eq!(func1(), 3);",
    );
}

#[test]
fn array_fill_writes_a_range() {
    // `array.fill` writes `len` copies of a value at an offset, leaving the rest
    // untouched.
    compile_run(
        "gc_array_fill",
        r#"(module
            (type $arr (array (mut i32)))
            (func (export "inside") (result i32)
              (local $r (ref $arr))
              i32.const 0 i32.const 5 array.new $arr local.set $r
              local.get $r i32.const 1 i32.const 7 i32.const 3 array.fill $arr
              local.get $r i32.const 2 array.get $arr)
            (func (export "outside") (result i32)
              (local $r (ref $arr))
              i32.const 0 i32.const 5 array.new $arr local.set $r
              local.get $r i32.const 1 i32.const 7 i32.const 3 array.fill $arr
              local.get $r i32.const 0 array.get $arr))"#,
        "assert_eq!(func0(), 7); assert_eq!(func1(), 0);",
    );
}

#[test]
fn array_copy_moves_a_range_between_arrays() {
    // `array.copy` copies `len` elements from the source array/offset to the
    // destination array/offset.
    compile_run(
        "gc_array_copy",
        r#"(module
            (type $arr (array (mut i32)))
            (func (export "f") (result i32)
              (local $src (ref $arr)) (local $dst (ref $arr))
              i32.const 0 i32.const 5 array.new $arr local.set $src
              local.get $src i32.const 0 i32.const 10 array.set $arr
              local.get $src i32.const 1 i32.const 20 array.set $arr
              local.get $src i32.const 2 i32.const 30 array.set $arr
              i32.const 0 i32.const 5 array.new $arr local.set $dst
              local.get $dst i32.const 1 local.get $src i32.const 0 i32.const 3 array.copy $arr $arr
              local.get $dst i32.const 2 array.get $arr))"#,
        "assert_eq!(func0(), 20);",
    );
}
