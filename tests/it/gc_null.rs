//! End-to-end tests for GC phase 4c-2: null and equality reference operators.
//! `ref.eq` compares references by identity, `ref.as_non_null` narrows a
//! nullable reference (trapping on null), and `br_on_null`/`br_on_non_null`
//! branch on nullness. `ref.null`/`ref.is_null` also work on abstract GC heap
//! types (`any`/`eq`/`struct`/`none`). Mixing an `i31` into an `anyref` is out
//! of scope here (deferred value-model unification).

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
fn ref_eq_compares_by_identity() {
    // `ref.eq` is 1 for two handles to the same object and 0 for distinct
    // objects (even with equal contents).
    compile_run(
        "gc_ref_eq",
        r#"(module
            (type $s (struct (field i32)))
            (func (export "same") (result i32)
              (local $r (ref $s))
              i32.const 1 struct.new $s local.set $r
              local.get $r local.get $r ref.eq)
            (func (export "diff") (result i32)
              i32.const 1 struct.new $s
              i32.const 1 struct.new $s
              ref.eq))"#,
        "assert_eq!(func0(), 1); assert_eq!(func1(), 0);",
    );
}

#[test]
fn ref_as_non_null_passes_through_a_non_null_reference() {
    compile_run(
        "gc_as_non_null_ok",
        r#"(module
            (type $s (struct (field i32)))
            (func $get (param (ref null $s)) (result i32)
              local.get 0 ref.as_non_null struct.get $s 0)
            (func (export "ok") (result i32)
              i32.const 9 struct.new $s call $get))"#,
        "assert_eq!(func1(), 9);",
    );
}

#[test]
fn ref_as_non_null_traps_on_null() {
    expect_trap(
        "gc_as_non_null_trap",
        r#"(module
            (type $s (struct (field i32)))
            (func (export "cf")
              ref.null $s ref.as_non_null drop))"#,
        "func0();",
    );
}

#[test]
fn br_on_null_branches_when_null() {
    // `br_on_null $l` branches (consuming the ref) when it is null, else leaves
    // the non-null ref on the stack.
    compile_run(
        "gc_br_on_null",
        r#"(module
            (type $s (struct (field i32)))
            (func $f (param (ref null $s)) (result i32)
              (block $isnull
                local.get 0
                br_on_null $isnull
                struct.get $s 0
                return)
              i32.const -1)
            (func (export "nn") (result i32)
              i32.const 5 struct.new $s call $f)
            (func (export "n") (result i32)
              ref.null $s call $f))"#,
        "assert_eq!(func1(), 5); assert_eq!(func2(), -1);",
    );
}

#[test]
fn br_on_non_null_branches_when_present() {
    // `br_on_non_null $l` branches (carrying the non-null ref) when present, else
    // falls through with the null consumed.
    compile_run(
        "gc_br_on_non_null",
        r#"(module
            (type $s (struct (field i32)))
            (func $f (param (ref null $s)) (result i32)
              (block $nonnull (result (ref $s))
                local.get 0
                br_on_non_null $nonnull
                i32.const -1 return)
              struct.get $s 0)
            (func (export "nn") (result i32)
              i32.const 5 struct.new $s call $f)
            (func (export "n") (result i32)
              ref.null $s call $f))"#,
        "assert_eq!(func1(), 5); assert_eq!(func2(), -1);",
    );
}

#[test]
fn ref_null_and_is_null_on_abstract_gc_heap_types() {
    // `ref.null any` / `ref.null struct` produce the managed null handle, which
    // `ref.is_null` reports as null.
    compile_run(
        "gc_abstract_null",
        r#"(module
            (func (export "an") (result i32) ref.null any ref.is_null)
            (func (export "sn") (result i32) ref.null struct ref.is_null))"#,
        "assert_eq!(func0(), 1); assert_eq!(func1(), 1);",
    );
}
