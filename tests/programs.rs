//! End-to-end tests that transpile whole small *programs* rather than single
//! instructions, exercising how calls, loops, branches, comparisons, and linear
//! memory interact. Each module is compiled with `rustc -D warnings` and run;
//! the assertions check computed results against hand-computed expectations.

mod common;

use common::compile_run;

#[test]
fn recursive_fibonacci() {
    // Recursion + early-return-style if/else + arithmetic.
    compile_run(
        "prog_fib",
        r#"
        (module
          (func $fib (export "fib") (param i32) (result i32)
            (if (result i32) (i32.lt_s (local.get 0) (i32.const 2))
              (then (local.get 0))
              (else (i32.add
                (call $fib (i32.sub (local.get 0) (i32.const 1)))
                (call $fib (i32.sub (local.get 0) (i32.const 2))))))))
        "#,
        "assert_eq!(func0(0), 0);\n    \
         assert_eq!(func0(1), 1);\n    \
         assert_eq!(func0(7), 13);\n    \
         assert_eq!(func0(10), 55);",
    );
}

#[test]
fn iterative_factorial_with_loop() {
    // A counted loop with a block/loop pair, an accumulator local, and a br_if
    // exit condition.
    compile_run(
        "prog_fact",
        r#"
        (module
          (func (export "fact") (param $n i32) (result i32)
            (local $acc i32) (local $i i32)
            (local.set $acc (i32.const 1))
            (local.set $i (i32.const 1))
            (block $done
              (loop $loop
                (br_if $done (i32.gt_s (local.get $i) (local.get $n)))
                (local.set $acc (i32.mul (local.get $acc) (local.get $i)))
                (local.set $i (i32.add (local.get $i) (i32.const 1)))
                (br $loop)))
            (local.get $acc)))
        "#,
        "assert_eq!(func0(0), 1);\n    \
         assert_eq!(func0(1), 1);\n    \
         assert_eq!(func0(5), 120);",
    );
}

#[test]
fn euclidean_gcd() {
    // Loop with a three-way local shuffle and unsigned remainder.
    compile_run(
        "prog_gcd",
        r#"
        (module
          (func (export "gcd") (param $a i32) (param $b i32) (result i32)
            (local $t i32)
            (block $done
              (loop $loop
                (br_if $done (i32.eqz (local.get $b)))
                (local.set $t (local.get $b))
                (local.set $b (i32.rem_u (local.get $a) (local.get $b)))
                (local.set $a (local.get $t))
                (br $loop)))
            (local.get $a)))
        "#,
        "assert_eq!(func0(48, 36), 12);\n    \
         assert_eq!(func0(17, 5), 1);\n    \
         assert_eq!(func0(100, 0), 100);",
    );
}

#[test]
fn bubble_sort_over_linear_memory() {
    // Nested loops, memory load/store at computed addresses, and a conditional
    // swap — the classic in-place sort. `put`/`get` expose the i32 array at
    // byte offset `k*4`.
    compile_run(
        "prog_sort",
        r#"
        (module
          (memory 1)
          (func (export "sort") (param $n i32)
            (local $i i32) (local $j i32) (local $a i32) (local $b i32)
            (local.set $i (i32.const 0))
            (block $outer_done
              (loop $outer
                (br_if $outer_done (i32.ge_s (local.get $i) (local.get $n)))
                (local.set $j (i32.const 0))
                (block $inner_done
                  (loop $inner
                    (br_if $inner_done (i32.ge_s (local.get $j)
                      (i32.sub (i32.sub (local.get $n) (local.get $i)) (i32.const 1))))
                    (local.set $a (i32.load (i32.mul (local.get $j) (i32.const 4))))
                    (local.set $b (i32.load
                      (i32.mul (i32.add (local.get $j) (i32.const 1)) (i32.const 4))))
                    (if (i32.gt_s (local.get $a) (local.get $b))
                      (then
                        (i32.store (i32.mul (local.get $j) (i32.const 4)) (local.get $b))
                        (i32.store
                          (i32.mul (i32.add (local.get $j) (i32.const 1)) (i32.const 4))
                          (local.get $a))))
                    (local.set $j (i32.add (local.get $j) (i32.const 1)))
                    (br $inner)))
                (local.set $i (i32.add (local.get $i) (i32.const 1)))
                (br $outer))))
          (func (export "get") (param $k i32) (result i32)
            (i32.load (i32.mul (local.get $k) (i32.const 4))))
          (func (export "put") (param $k i32) (param $v i32)
            (i32.store (i32.mul (local.get $k) (i32.const 4)) (local.get $v))))
        "#,
        "let mut inst = Instance::new();\n    \
         let input = [5, 3, 8, 1, 9];\n    \
         for (k, v) in input.iter().enumerate() { inst.func2(k as i32, *v); }\n    \
         inst.func0(input.len() as i32);\n    \
         let sorted: Vec<i32> = (0..input.len() as i32).map(|k| inst.func1(k)).collect();\n    \
         assert_eq!(sorted, vec![1, 3, 5, 8, 9]);",
    );
}

#[test]
fn br_table_dispatched_calculator() {
    // A `br_table` switch selects one of three operations, with a default arm
    // for out-of-range selectors — the canonical structured-switch idiom.
    compile_run(
        "prog_calc",
        r#"
        (module
          (func (export "calc") (param $op i32) (param $a i32) (param $b i32) (result i32)
            (block $default
              (block $c2
                (block $c1
                  (block $c0
                    (br_table $c0 $c1 $c2 $default (local.get $op)))
                  (return (i32.add (local.get $a) (local.get $b))))
                (return (i32.sub (local.get $a) (local.get $b))))
              (return (i32.mul (local.get $a) (local.get $b))))
            (i32.const -1)))
        "#,
        "assert_eq!(func0(0, 7, 3), 10);\n    \
         assert_eq!(func0(1, 7, 3), 4);\n    \
         assert_eq!(func0(2, 7, 3), 21);\n    \
         assert_eq!(func0(9, 7, 3), -1);",
    );
}
