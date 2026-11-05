//! `try_table` (the exception-handling proposal's structured form): its
//! `catch $tag $label` / `catch_all $label` clauses branch to an enclosing label,
//! lowered through the legacy `try`/`catch` machinery.

use crate::common::{compile_run, expect_trap};

#[test]
fn catch_branches_to_an_outer_block_with_the_payload() {
    compile_run(
        "tt_catch_payload",
        r#"(module
            (tag $e (param i32))
            (func (export "f") (param i32) (result i32)
              (block $h (result i32)
                (try_table (result i32) (catch $e $h)
                  (throw $e (local.get 0))))))"#,
        "assert_eq!(func0(42), 42); assert_eq!(func0(-7), -7);",
    );
}

#[test]
fn catch_all_branches_to_the_label() {
    compile_run(
        "tt_catch_all",
        r#"(module
            (tag $e)
            (func (export "f") (result i32)
              (block $h
                (try_table (catch_all $h)
                  (throw $e))
                (return (i32.const 1)))
              (i32.const 7)))"#,
        "assert_eq!(func0(), 7);",
    );
}

#[test]
fn the_body_fallthrough_value_is_the_result_when_nothing_is_thrown() {
    compile_run(
        "tt_fallthrough",
        r#"(module
            (tag $e (param i32))
            (func (export "f") (result i32)
              (block $h (result i32)
                (try_table (result i32) (catch $e $h)
                  (i32.const 5)))))"#,
        "assert_eq!(func0(), 5);",
    );
}

#[test]
fn the_first_matching_clause_wins() {
    compile_run(
        "tt_clause_order",
        r#"(module
            (tag $a (param i32))
            (tag $b (param i32))
            (func (export "f") (param i32) (result i32)
              (block $hb (result i32)
                (block $ha (result i32)
                  (try_table (result i32) (catch $a $ha) (catch $b $hb)
                    (if (local.get 0)
                      (then (throw $a (i32.const 10)))
                      (else (throw $b (i32.const 20))))
                    (i32.const 0))
                  (return))
                (return (i32.add (i32.const 100))))
              (i32.add (i32.const 1000))))"#,
        "assert_eq!(func0(1), 110); assert_eq!(func0(0), 1020);",
    );
}

#[test]
fn an_unmatched_tag_propagates_out() {
    expect_trap(
        "tt_unmatched",
        r#"(module
            (tag $a)
            (tag $b)
            (func (export "f")
              (block $h
                (try_table (catch $a $h)
                  (throw $b)))))"#,
        "func0();",
    );
}

#[test]
fn a_callee_throw_is_caught() {
    compile_run(
        "tt_callee",
        r#"(module
            (tag $e (param i32))
            (func $thrower (throw $e (i32.const 9)))
            (func (export "f") (result i32)
              (block $h (result i32)
                (try_table (result i32) (catch $e $h)
                  (call $thrower)
                  (i32.const 0)))))"#,
        "assert_eq!(func1(), 9);",
    );
}

#[test]
fn a_catch_may_branch_to_a_loop_label() {
    compile_run(
        "tt_loop_label",
        r#"(module
            (tag $e)
            (func (export "f") (result i32)
              (local $n i32)
              (loop $again
                (local.set $n (i32.add (local.get $n) (i32.const 1)))
                (try_table (catch_all $again)
                  (if (i32.lt_u (local.get $n) (i32.const 3))
                    (then (throw $e)))))
              (local.get $n)))"#,
        "assert_eq!(func0(), 3);",
    );
}

#[test]
fn a_catch_may_branch_to_the_function_label() {
    compile_run(
        "tt_function_label",
        r#"(module
            (tag $e (param i32))
            (func (export "f") (param i32) (result i32)
              (try_table (catch $e 0)
                (throw $e (local.get 0)))
              (i32.const 0)))"#,
        "assert_eq!(func0(31), 31);",
    );
}

#[test]
fn try_tables_nest_and_the_inner_miss_reaches_the_outer() {
    compile_run(
        "tt_nested",
        r#"(module
            (tag $a)
            (tag $b (param i32))
            (func (export "f") (result i32)
              (block $outer (result i32)
                (try_table (result i32) (catch $b $outer)
                  (block $inner
                    (try_table (catch $a $inner)
                      (throw $b (i32.const 3))))
                  (i32.const 1)))))"#,
        "assert_eq!(func0(), 3);",
    );
}

#[test]
fn a_try_table_inside_a_legacy_try_body() {
    compile_run(
        "tt_in_legacy_try",
        r#"(module
            (tag $a)
            (tag $b)
            (func (export "f") (result i32)
              try (result i32)
                (block $h
                  (try_table (catch $a $h)
                    (throw $a)))
                i32.const 4
              catch_all
                i32.const 8
              end))"#,
        "assert_eq!(func0(), 4);",
    );
}

#[test]
fn a_catch_all_before_a_tag_clause_makes_the_later_clause_dead() {
    // The first matching clause wins, so the `catch $e` after a `catch_all` can
    // never run; it must not produce an unreachable match arm in the output.
    compile_run(
        "tt_catch_all_first",
        r#"(module
            (tag $e (param i32))
            (func (export "f") (result i32)
              (block $all
                (block $t (result i32)
                  (try_table (catch_all $all) (catch $e $t)
                    (throw $e (i32.const 5)))
                  (return (i32.const -1)))
                (return))
              (i32.const 9)))"#,
        "assert_eq!(func0(), 9);",
    );
}

#[test]
fn a_repeated_tag_clause_uses_the_first_label() {
    compile_run(
        "tt_repeated_tag",
        r#"(module
            (tag $e (param i32))
            (func (export "f") (result i32)
              (block $b (result i32)
                (block $a (result i32)
                  (try_table (catch $e $a) (catch $e $b)
                    (throw $e (i32.const 5)))
                  (return (i32.const -1)))
                (return (i32.add (i32.const 100))))
              (i32.add (i32.const 1000))))"#,
        "assert_eq!(func0(), 105);",
    );
}
