//! A `br_table` inside a `try` body whose arms leave the body (legacy exception
//! handling): each escaping arm is re-issued outside the protected closure.

use crate::common::compile_run;

#[test]
fn every_arm_escaping_the_try_reaches_its_target() {
    compile_run(
        "bt_try_all_escape",
        r#"(module
            (func (export "f") (param i32) (result i32)
              (block $b0
                (block $b1
                  try
                    (br_table $b0 $b1 $b0 (local.get 0))
                  catch_all
                  end
                  (return (i32.const 11)))
                (return (i32.const 22)))
              (i32.const 33)))"#,
        "assert_eq!(func0(0), 33); assert_eq!(func0(1), 22); assert_eq!(func0(2), 33); \
         assert_eq!(func0(9), 33);",
    );
}

#[test]
fn an_arm_staying_inside_the_try_mixes_with_escaping_arms() {
    compile_run(
        "bt_try_mixed",
        r#"(module
            (func (export "f") (param i32) (result i32)
              (local $r i32)
              (block $b0
                (block $b1
                  try
                    (block $in (br_table $in $b1 $b0 (local.get 0)))
                    (local.set $r (i32.const 5))
                  catch_all
                  end
                  (br $b0))
                (local.set $r (i32.const 11)))
              (local.get $r)))"#,
        "assert_eq!(func0(0), 5); assert_eq!(func0(1), 11); assert_eq!(func0(2), 0); \
         assert_eq!(func0(9), 0);",
    );
}

#[test]
fn an_escaping_br_table_carries_its_value() {
    compile_run(
        "bt_try_value",
        r#"(module
            (func (export "f") (param i32) (result i32)
              (block $c (result i32)
                (i32.add
                  (block $a (result i32)
                    try
                      (i32.const 42) (local.get 0) (br_table $a $c)
                    catch_all
                    end
                    (i32.const 0))
                  (i32.const 1)))))"#,
        "assert_eq!(func0(0), 43); assert_eq!(func0(5), 42);",
    );
}

#[test]
fn an_arm_can_leave_more_than_one_try() {
    compile_run(
        "bt_try_nested",
        r#"(module
            (func (export "f") (param i32) (result i32)
              (block $o
                try
                  (block $m
                    try
                      (br_table $m $o (local.get 0))
                    catch_all
                    end
                    (return (i32.const 10)))
                  (return (i32.const 20))
                catch_all
                end
                (return (i32.const 40)))
              (i32.const 30)))"#,
        "assert_eq!(func0(0), 20); assert_eq!(func0(3), 30);",
    );
}

#[test]
fn an_escaping_arm_can_continue_a_loop() {
    compile_run(
        "bt_try_loop",
        r#"(module
            (func (export "f") (param i32) (result i32)
              (local $n i32)
              (block $exit
                (loop $l
                  (local.set $n (i32.add (local.get $n) (i32.const 1)))
                  try
                    (br_table $exit $l (i32.lt_u (local.get $n) (local.get 0)))
                  catch_all
                  end))
              (local.get $n)))"#,
        "assert_eq!(func0(5), 5); assert_eq!(func0(0), 1);",
    );
}

#[test]
fn a_thrown_exception_is_still_caught_around_an_escaping_br_table() {
    compile_run(
        "bt_try_throw",
        r#"(module
            (tag $e)
            (func (export "f") (param i32) (result i32)
              (block $out
                try
                  (if (local.get 0) (then (throw $e)))
                  (br_table $out $out (local.get 0))
                catch_all
                  (return (i32.const 7))
                end
                (return (i32.const 1)))
              (i32.const 2)))"#,
        "assert_eq!(func0(0), 2); assert_eq!(func0(1), 7);",
    );
}
