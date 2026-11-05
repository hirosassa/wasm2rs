//! `exnref` values: the `_ref` forms of `try_table` catch clauses capture the
//! caught exception as a handle, and `throw_ref` raises it again.

use crate::common::{compile_run, expect_trap};

#[test]
fn a_null_exnref_is_null() {
    compile_run(
        "exn_null",
        r#"(module
            (func (export "f") (result i32)
              (ref.is_null (ref.null exn))))"#,
        "assert_eq!(func0(), 1);",
    );
}

#[test]
fn an_exnref_local_defaults_to_null_and_passes_through_a_call() {
    compile_run(
        "exn_local",
        r#"(module
            (func $id (param exnref) (result exnref) (local.get 0))
            (func (export "f") (result i32)
              (local exnref)
              (i32.add
                (ref.is_null (local.get 0))
                (ref.is_null (call $id (local.get 0))))))"#,
        "assert_eq!(func1(), 2);",
    );
}

#[test]
fn catch_all_ref_captures_the_exception_and_throw_ref_rethrows_it() {
    compile_run(
        "exn_rethrow",
        r#"(module
            (tag $e (param i32))
            (func (export "f") (param i32) (result i32)
              (block $outer (result i32)
                (try_table (result i32) (catch $e $outer)
                  (throw_ref
                    (block $h (result exnref)
                      (try_table (catch_all_ref $h) (throw $e (local.get 0)))
                      (unreachable)))))))"#,
        "assert_eq!(func0(9), 9); assert_eq!(func0(-3), -3);",
    );
}

#[test]
fn catch_ref_pushes_the_payload_then_the_exnref() {
    compile_run(
        "exn_catch_ref",
        r#"(module
            (tag $e (param i32))
            (func (export "f") (param i32) (result i32)
              (local $x exnref) (local $p i32)
              (block $h (result i32 exnref)
                (try_table (catch_ref $e $h) (throw $e (local.get 0)))
                (unreachable))
              (local.set $x)
              (local.set $p)
              (i32.add (local.get $p) (i32.add (i32.const 1) (ref.is_null (local.get $x))))))"#,
        "assert_eq!(func0(4), 5);",
    );
}

#[test]
fn a_caught_exnref_can_be_rethrown_more_than_once() {
    compile_run(
        "exn_twice",
        r#"(module
            (tag $e (param i32))
            (func $catch_it (param i32) (result exnref)
              (block $h (result exnref)
                (try_table (catch_all_ref $h) (throw $e (local.get 0)))
                (unreachable)))
            (func $caught (param exnref) (result i32)
              (block $o (result i32)
                (try_table (result i32) (catch $e $o)
                  (throw_ref (local.get 0)))))
            (func (export "f") (param i32) (result i32)
              (local $x exnref)
              (local.set $x (call $catch_it (local.get 0)))
              (i32.add (call $caught (local.get $x)) (call $caught (local.get $x)))))"#,
        "assert_eq!(func2(5), 10);",
    );
}

#[test]
fn throw_ref_of_a_null_reference_traps() {
    expect_trap(
        "exn_throw_null",
        r#"(module
            (func (export "f") (throw_ref (ref.null exn))))"#,
        "func0();",
    );
}

#[test]
fn throw_ref_is_caught_by_a_legacy_catch_all() {
    compile_run(
        "exn_legacy",
        r#"(module
            (tag $e)
            (func (export "f") (result i32)
              (local $x exnref)
              (local.set $x
                (block $h (result exnref)
                  (try_table (catch_all_ref $h) (throw $e))
                  (unreachable)))
              try (result i32)
                (throw_ref (local.get $x))
              catch_all
                (i32.const 7)
              end))"#,
        "assert_eq!(func0(), 7);",
    );
}

#[test]
fn an_exnref_global_holds_a_caught_exception() {
    compile_run(
        "exn_global",
        r#"(module
            (tag $e (param i32))
            (global $g (mut exnref) (ref.null exn))
            (func (export "f") (param i32) (result i32)
              (local $before i32)
              (local.set $before (ref.is_null (global.get $g)))
              (global.set $g
                (block $h (result exnref)
                  (try_table (catch_all_ref $h) (throw $e (local.get 0)))
                  (unreachable)))
              (i32.add (local.get $before)
                (i32.mul (i32.const 10) (ref.is_null (global.get $g))))))"#,
        "let mut i = Instance::new(); assert_eq!(i.func0(3), 1);",
    );
}
