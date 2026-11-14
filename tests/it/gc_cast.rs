//! End-to-end tests for GC phase 4c-1: runtime downcasts and subtyping.
//! `ref.test` reports whether a reference's runtime type is a subtype of the
//! target; `ref.cast` narrows or traps; `br_on_cast`/`br_on_cast_fail` branch on
//! the outcome. Each heap object carries its concrete type id at runtime, and a
//! type's declared supertype chain drives the check.

use crate::common;

use common::{compile_run, expect_trap};

// A two-level hierarchy reused across the tests: `$b` is a subtype of `$a`.
const HIER: &str = r#"
    (type $a (sub (struct (field i32))))
    (type $b (sub $a (struct (field i32) (field i32))))"#;

// A three-level hierarchy: `$c <: $b <: $a`, each level adding a field.
const HIER3: &str = r#"
    (type $a (sub (struct (field i32))))
    (type $b (sub $a (struct (field i32) (field i32))))
    (type $c (sub $b (struct (field i32) (field i32) (field i32))))"#;

#[test]
fn ref_cast_narrows_two_levels_down_a_hierarchy() {
    // A `$c` value seen statically as the top supertype `$a` casts straight down
    // two levels to `(ref $c)`, letting the deepest field (index 2) be read.
    compile_run(
        "gc_cast_deep",
        &format!(
            r#"(module {HIER3}
                (func (export "deep") (result i32)
                  (local $r (ref $a))
                  (local.set $r
                    (struct.new $c (i32.const 10) (i32.const 20) (i32.const 30)))
                  (struct.get $c 2 (ref.cast (ref $c) (local.get $r)))))"#
        ),
        "assert_eq!(func0(), 30);",
    );
}

#[test]
fn ref_test_matches_a_middle_supertype() {
    // A `$c` value is a subtype of the *middle* type `$b`, so `ref.test (ref $b)`
    // reports 1 even though the value's runtime type is the deeper `$c`.
    compile_run(
        "gc_test_middle",
        &format!(
            r#"(module {HIER3}
                (func (export "mid") (result i32)
                  (local $r (ref $a))
                  (local.set $r
                    (struct.new $c (i32.const 1) (i32.const 2) (i32.const 3)))
                  (ref.test (ref $b) (local.get $r))))"#
        ),
        "assert_eq!(func0(), 1);",
    );
}

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

// Abstract heap types: `ref.test` answers must follow the spec for every kind
// of GC value. `$probe` packs the answers into a bitmask so that one module
// checks all combinations: bits 0-5 are the non-null tests (any, eq, i31,
// struct, array, none), bits 6-11 the same tests on the nullable target.
const PROBE: &str = r#"
    (type $s (struct (field i32)))
    (type $arr (array i32))
    (func $probe (param anyref) (result i32)
      i32.const 0
      (ref.test (ref any) (local.get 0)) (i32.const 0) i32.shl i32.or
      (ref.test (ref eq) (local.get 0)) (i32.const 1) i32.shl i32.or
      (ref.test (ref i31) (local.get 0)) (i32.const 2) i32.shl i32.or
      (ref.test (ref struct) (local.get 0)) (i32.const 3) i32.shl i32.or
      (ref.test (ref array) (local.get 0)) (i32.const 4) i32.shl i32.or
      (ref.test (ref none) (local.get 0)) (i32.const 5) i32.shl i32.or
      (ref.test (ref null any) (local.get 0)) (i32.const 6) i32.shl i32.or
      (ref.test (ref null eq) (local.get 0)) (i32.const 7) i32.shl i32.or
      (ref.test (ref null i31) (local.get 0)) (i32.const 8) i32.shl i32.or
      (ref.test (ref null struct) (local.get 0)) (i32.const 9) i32.shl i32.or
      (ref.test (ref null array) (local.get 0)) (i32.const 10) i32.shl i32.or
      (ref.test (ref null none) (local.get 0)) (i32.const 11) i32.shl i32.or
    )
"#;

#[test]
fn ref_test_against_abstract_heap_types_follows_the_spec_for_every_value_kind() {
    // struct: any, eq, struct (nullable or not) -> 1 + 2 + 8 = 11
    // array:  any, eq, array                    -> 1 + 2 + 16 = 19
    // i31:    any, eq, i31                      -> 1 + 2 + 4 = 7
    // null:   only the nullable targets match   -> 63 << 6
    compile_run(
        "gc_test_abstract_matrix",
        &format!(
            r#"(module {PROBE}
                (func (export "on_struct") (result i32)
                  (call $probe (struct.new $s (i32.const 1))))
                (func (export "on_array") (result i32)
                  (call $probe (array.new $arr (i32.const 0) (i32.const 2))))
                (func (export "on_i31") (result i32)
                  (call $probe (ref.i31 (i32.const 5))))
                (func (export "on_null") (result i32)
                  (call $probe (ref.null any))))"#
        ),
        "assert_eq!(func1(), 11 | (11 << 6));
         assert_eq!(func2(), 19 | (19 << 6));
         assert_eq!(func3(), 7 | (7 << 6));
         assert_eq!(func4(), 63 << 6);",
    );
}

#[test]
fn ref_test_struct_is_false_in_a_module_that_declares_no_struct_type() {
    // With no struct type at all, no heap object can be a struct: the test is
    // false for an array and for i31, and only the nullable form matches null.
    compile_run(
        "gc_test_struct_without_structs",
        r#"(module
            (type $arr (array i32))
            (func $is_struct (param anyref) (result i32)
              (ref.test (ref struct) (local.get 0)))
            (func $is_struct_or_null (param anyref) (result i32)
              (ref.test (ref null struct) (local.get 0)))
            (func (export "array") (result i32)
              (call $is_struct (array.new $arr (i32.const 0) (i32.const 1))))
            (func (export "i31") (result i32)
              (call $is_struct (ref.i31 (i32.const 1))))
            (func (export "null") (result i32)
              (call $is_struct_or_null (ref.null any)))
            (func (export "non_null") (result i32)
              (call $is_struct (ref.null any))))"#,
        "assert_eq!(func2(), 0); assert_eq!(func3(), 0);
         assert_eq!(func4(), 1); assert_eq!(func5(), 0);",
    );
}

#[test]
fn ref_test_array_is_false_in_a_module_that_declares_no_array_type() {
    compile_run(
        "gc_test_array_without_arrays",
        r#"(module
            (type $s (struct (field i32)))
            (func $is_array (param anyref) (result i32)
              (ref.test (ref array) (local.get 0)))
            (func (export "struct") (result i32)
              (call $is_array (struct.new $s (i32.const 1))))
            (func (export "i31") (result i32)
              (call $is_array (ref.i31 (i32.const 1)))))"#,
        "assert_eq!(func1(), 0); assert_eq!(func2(), 0);",
    );
}

#[test]
fn ref_cast_to_abstract_heap_types_returns_the_matching_value() {
    // Each cast that the spec allows returns the value (checked by a follow-up
    // operation on it); the casts on mismatched values are exercised below.
    compile_run(
        "gc_cast_abstract_ok",
        r#"(module
            (type $s (struct (field i32)))
            (type $arr (array i32))
            (func (export "struct") (result i32)
              (struct.get $s 0
                (ref.cast (ref $s)
                  (ref.cast (ref struct)
                    (ref.cast (ref eq)
                      (ref.cast (ref any) (struct.new $s (i32.const 11))))))))
            (func (export "array") (result i32)
              (array.len (ref.cast (ref array)
                (ref.cast (ref null eq) (array.new $arr (i32.const 0) (i32.const 3))))))
            (func (export "i31") (result i32)
              (i31.get_u (ref.cast (ref i31) (ref.i31 (i32.const 21)))))
            (func (export "null") (result i32)
              (ref.is_null (ref.cast (ref null none) (ref.null any)))))"#,
        "assert_eq!(func0(), 11); assert_eq!(func1(), 3);
         assert_eq!(func2(), 21); assert_eq!(func3(), 1);",
    );
}

fn cast_trap(name: &str, ty: &str, value: &str) {
    common::expect_trap_with(
        name,
        &format!(
            r#"(module
                (type $s (struct (field i32)))
                (type $arr (array i32))
                (func (export "go") (drop (ref.cast {ty} {value}))))"#
        ),
        "func0();",
        "ref.cast failed",
    );
}

#[test]
fn ref_cast_traps_when_the_value_is_not_of_the_abstract_target() {
    cast_trap(
        "gc_cast_struct_from_array",
        "(ref struct)",
        "(array.new $arr (i32.const 0) (i32.const 1))",
    );
    cast_trap(
        "gc_cast_struct_from_i31",
        "(ref struct)",
        "(ref.i31 (i32.const 1))",
    );
    cast_trap(
        "gc_cast_array_from_struct",
        "(ref array)",
        "(struct.new $s (i32.const 1))",
    );
    cast_trap(
        "gc_cast_i31_from_struct",
        "(ref i31)",
        "(struct.new $s (i32.const 1))",
    );
    cast_trap(
        "gc_cast_none_from_struct",
        "(ref null none)",
        "(struct.new $s (i32.const 1))",
    );
    cast_trap(
        "gc_cast_non_null_struct_from_null",
        "(ref struct)",
        "(ref.null any)",
    );
}
