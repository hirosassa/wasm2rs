//! End-to-end tests for GC phase 4c-1: runtime downcasts and subtyping.
//! `ref.test` reports whether a reference's runtime type is a subtype of the
//! target; `ref.cast` narrows or traps; `br_on_cast`/`br_on_cast_fail` branch on
//! the outcome. Each heap object carries its concrete type id at runtime, and a
//! type's declared supertype chain drives the check.

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

use common::{compile_run, expect_trap};

// A two-level hierarchy reused across the tests: `$b` is a subtype of `$a`.
const HIER: &str = r#"
    (type $a (sub (struct (field i32))))
    (type $b (sub $a (struct (field i32) (field i32))))"#;

#[test]
fn ref_test_respects_subtyping() {
    // `ref.test (ref $b)` is 1 for a value whose runtime type is `$b` (even when
    // statically seen as the supertype `$a`) and 0 for a plain `$a`.
    compile_run(
        "gc_ref_test",
        &format!(
            r#"(module {HIER}
                (func $test (param (ref $a)) (result i32)
                  local.get 0 ref.test (ref $b))
                (func (export "run_b") (result i32)
                  i32.const 1 i32.const 2 struct.new $b call $test)
                (func (export "run_a") (result i32)
                  i32.const 7 struct.new $a call $test))"#
        ),
        "assert_eq!(func1(), 1); assert_eq!(func2(), 0);",
    );
}

#[test]
fn ref_cast_succeeds_and_reads_field() {
    // A successful `ref.cast (ref $b)` narrows the reference so the subtype's
    // extra field can be read.
    compile_run(
        "gc_ref_cast_ok",
        &format!(
            r#"(module {HIER}
                (func $cast (param (ref $a)) (result i32)
                  local.get 0 ref.cast (ref $b) struct.get $b 1)
                (func (export "ok") (result i32)
                  i32.const 1 i32.const 42 struct.new $b call $cast))"#
        ),
        "assert_eq!(func1(), 42);",
    );
}

#[test]
fn ref_cast_failure_traps() {
    // Casting an `$a` (not a `$b`) to `(ref $b)` traps.
    expect_trap(
        "gc_ref_cast_trap",
        &format!(
            r#"(module {HIER}
                (func $cast (param (ref $a))
                  local.get 0 ref.cast (ref $b) drop)
                (func (export "cf")
                  i32.const 7 struct.new $a call $cast))"#
        ),
        "func1();",
    );
}

#[test]
fn br_on_cast_branches_when_the_cast_matches() {
    // `br_on_cast $l (ref $a) (ref $b)` branches (carrying the value as `(ref $b)`)
    // when the runtime type matches, else falls through with `(ref $a)`.
    compile_run(
        "gc_br_on_cast",
        &format!(
            r#"(module {HIER}
                (func $classify (param (ref $a)) (result i32)
                  (block $isb (result (ref $b))
                    local.get 0
                    br_on_cast $isb (ref $a) (ref $b)
                    drop i32.const 0 return)
                  drop i32.const 1)
                (func (export "cb") (result i32)
                  i32.const 1 i32.const 2 struct.new $b call $classify)
                (func (export "ca") (result i32)
                  i32.const 7 struct.new $a call $classify))"#
        ),
        "assert_eq!(func1(), 1); assert_eq!(func2(), 0);",
    );
}

#[test]
fn br_on_cast_fail_branches_when_the_cast_misses() {
    // `br_on_cast_fail $l (ref $a) (ref $b)` branches (carrying `(ref $a)`) when
    // the cast would fail, else falls through with the narrowed `(ref $b)`.
    compile_run(
        "gc_br_on_cast_fail",
        &format!(
            r#"(module {HIER}
                (func $classify (param (ref $a)) (result i32)
                  (block $nota (result (ref $a))
                    local.get 0
                    br_on_cast_fail $nota (ref $a) (ref $b)
                    drop i32.const 1 return)
                  drop i32.const 0)
                (func (export "cb") (result i32)
                  i32.const 1 i32.const 2 struct.new $b call $classify)
                (func (export "ca") (result i32)
                  i32.const 7 struct.new $a call $classify))"#
        ),
        "assert_eq!(func1(), 1); assert_eq!(func2(), 0);",
    );
}
