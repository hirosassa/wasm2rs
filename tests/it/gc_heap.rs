//! End-to-end tests for GC phase 4b: heap-allocated `struct` and `array`
//! objects. References become a managed handle (reference-counted), fields and
//! elements are read/written by index, packed (`i8`/`i16`) fields sign/zero-
//! extend on read, `array.len` reports the length, and dereferencing a null
//! reference traps. Reference cycles leak (no tracing collector), which is
//! acceptable for this phase.

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

use crate::common;

use common::{compile_run, expect_trap};

#[test]
fn struct_new_and_get_read_fields_back() {
    // `struct.new` builds an object from its fields (in order); `struct.get`
    // reads a field by index. A mutable i32 field and an immutable i64 field
    // round-trip independently.
    compile_run(
        "gc_struct_new_get",
        r#"(module
            (type $s (struct (field (mut i32)) (field i64)))
            (func (export "get_a") (param i32) (result i32)
              local.get 0 i64.const 100 struct.new $s struct.get $s 0)
            (func (export "get_b") (param i32) (result i64)
              local.get 0 i64.const 100 struct.new $s struct.get $s 1))"#,
        "assert_eq!(func0(5), 5); assert_eq!(func1(5), 100);",
    );
}

#[test]
fn struct_set_mutates_a_field() {
    // `struct.set` writes a mutable field; a later `struct.get` observes it.
    compile_run(
        "gc_struct_set",
        r#"(module
            (type $s (struct (field (mut i32))))
            (func (export "f") (param i32) (result i32)
              (local $r (ref $s))
              i32.const 0 struct.new $s local.set $r
              local.get $r local.get 0 struct.set $s 0
              local.get $r struct.get $s 0))"#,
        "assert_eq!(func0(42), 42);",
    );
}

#[test]
fn packed_struct_fields_sign_and_zero_extend() {
    // A packed `i8` field stores only the low byte; `struct.get_s` sign-extends
    // it while `struct.get_u` zero-extends. 0xFF -> -1 signed, 255 unsigned.
    compile_run(
        "gc_struct_packed",
        r#"(module
            (type $s (struct (field i8)))
            (func (export "s8") (param i32) (result i32)
              local.get 0 struct.new $s struct.get_s $s 0)
            (func (export "u8") (param i32) (result i32)
              local.get 0 struct.new $s struct.get_u $s 0))"#,
        "assert_eq!(func0(0xFF), -1); assert_eq!(func1(0xFF), 255); \
         assert_eq!(func0(1), 1);",
    );
}

#[test]
fn packed_i16_struct_field_sign_and_zero_extends() {
    // A packed `i16` field keeps only the low 16 bits; `struct.get_s` sign-extends
    // that half-word and `struct.get_u` zero-extends it. This exercises the 16-bit
    // mask/shift path, which is distinct from the `i8` case above (0xFFFF -> -1
    // signed, 65535 unsigned; a mid value like 0x1234 survives both reads).
    compile_run(
        "gc_struct_packed_i16",
        r#"(module
            (type $s (struct (field i16)))
            (func (export "s16") (param i32) (result i32)
              local.get 0 struct.new $s struct.get_s $s 0)
            (func (export "u16") (param i32) (result i32)
              local.get 0 struct.new $s struct.get_u $s 0))"#,
        "assert_eq!(func0(0xFFFF), -1); assert_eq!(func1(0xFFFF), 65535); \
         assert_eq!(func0(0x1234), 0x1234); assert_eq!(func1(0x1234), 0x1234);",
    );
}

#[test]
fn array_new_get_set_and_len() {
    // `array.new` fills `size` elements with an initial value; `array.len`
    // reports the size; `array.set`/`array.get` write and read by index.
    compile_run(
        "gc_array",
        r#"(module
            (type $a (array (mut i32)))
            (func (export "len") (result i32)
              i32.const 7 i32.const 5 array.new $a array.len)
            (func (export "roundtrip") (param i32) (result i32)
              (local $r (ref $a))
              i32.const 0 i32.const 3 array.new $a local.set $r
              local.get $r i32.const 1 local.get 0 array.set $a
              local.get $r i32.const 1 array.get $a)
            (func (export "untouched") (result i32)
              i32.const 7 i32.const 5 array.new $a i32.const 4 array.get $a))"#,
        "assert_eq!(func0(), 5); assert_eq!(func1(9), 9); assert_eq!(func2(), 7);",
    );
}

#[test]
fn struct_field_holds_a_reference() {
    // A field typed `(ref $inner)` stores another heap object; reading it back
    // and dereferencing yields the nested value. Exercises ref-typed fields.
    compile_run(
        "gc_nested_ref",
        r#"(module
            (type $inner (struct (field i32)))
            (type $outer (struct (field (ref $inner))))
            (func (export "f") (param i32) (result i32)
              local.get 0 struct.new $inner
              struct.new $outer
              struct.get $outer 0
              struct.get $inner 0))"#,
        "assert_eq!(func0(7), 7);",
    );
}

#[test]
fn cyclic_references_build_and_read_back() {
    // Two nodes point at each other through a mutable `(ref null $node)` field,
    // forming a cycle. Building it exercises storing a reference *local* into a
    // struct field (`struct.new`/`struct.set`) and reading a local again
    // afterwards. The cycle leaks (no tracing collector, per this phase), which
    // is acceptable; the point is that it compiles and reads back correctly:
    // a -> b -> a, so `a.next.next.val` is `a.val` (11).
    compile_run(
        "gc_cycle",
        r#"(module
            (type $node (struct (field (mut (ref null $node))) (field i32)))
            (func (export "f") (result i32)
              (local $a (ref $node)) (local $b (ref $node))
              (local.set $a (struct.new $node (ref.null $node) (i32.const 11)))
              (local.set $b (struct.new $node (local.get $a) (i32.const 22)))
              (struct.set $node 0 (local.get $a) (local.get $b))
              (struct.get $node 1
                (struct.get $node 0
                  (struct.get $node 0 (local.get $a))))))"#,
        "assert_eq!(func0(), 11);",
    );
}

#[test]
fn get_on_null_reference_traps() {
    // Dereferencing a null reference (`struct.get` on `ref.null`) traps.
    expect_trap(
        "gc_null_deref",
        r#"(module
            (type $s (struct (field i32)))
            (func (export "f") (result i32)
              ref.null $s struct.get $s 0))"#,
        "func0();",
    );
}
