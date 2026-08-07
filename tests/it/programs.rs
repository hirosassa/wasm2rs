//! End-to-end tests that transpile whole small *programs* rather than single
//! instructions, exercising how calls, loops, branches, comparisons, and linear
//! memory interact. Each module is compiled with `rustc -D warnings` and run;
//! the assertions check computed results against hand-computed expectations.

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
fn stack_bytecode_interpreter() {
    // A whole little *program*: a stack-machine bytecode interpreter. The
    // fetch/decode/dispatch/execute loop combines a `loop`, a `br_table` opcode
    // switch, six locals (pc, sp, decoded op/arg, two ALU temps), and a value
    // stack held in linear memory. Each instruction occupies two i32 words
    // (opcode, operand) at byte `pc*8`; the value stack lives at byte 1024.
    //
    // Opcodes: 0 PUSH imm, 1 ADD, 2 MUL, 3 HALT (returns the stack top). The
    // driver assembles `PUSH 3; PUSH 4; ADD; PUSH 5; MUL; HALT`, i.e.
    // (3 + 4) * 5 = 35.
    compile_run(
        "prog_bytecode_vm",
        r#"
        (module
          (memory 1)
          (func (export "set_instr") (param $i i32) (param $op i32) (param $arg i32)
            (i32.store (i32.mul (local.get $i) (i32.const 8)) (local.get $op))
            (i32.store
              (i32.add (i32.mul (local.get $i) (i32.const 8)) (i32.const 4))
              (local.get $arg)))
          (func (export "run") (result i32)
            (local $pc i32) (local $sp i32) (local $op i32) (local $arg i32)
            (local $a i32) (local $b i32)
            (loop $loop
              (local.set $op (i32.load (i32.mul (local.get $pc) (i32.const 8))))
              (local.set $arg
                (i32.load (i32.add (i32.mul (local.get $pc) (i32.const 8)) (i32.const 4))))
              (block $default
                (block $halt
                  (block $mul
                    (block $add
                      (block $push
                        (br_table $push $add $mul $halt $default (local.get $op)))
                      ;; PUSH: stack[sp] = arg; sp += 1; pc += 1
                      (i32.store
                        (i32.add (i32.const 1024) (i32.mul (local.get $sp) (i32.const 4)))
                        (local.get $arg))
                      (local.set $sp (i32.add (local.get $sp) (i32.const 1)))
                      (local.set $pc (i32.add (local.get $pc) (i32.const 1)))
                      (br $loop))
                    ;; ADD: b = pop; a = pop; push a + b
                    (local.set $b (i32.load
                      (i32.add (i32.const 1024)
                        (i32.mul (i32.sub (local.get $sp) (i32.const 1)) (i32.const 4)))))
                    (local.set $a (i32.load
                      (i32.add (i32.const 1024)
                        (i32.mul (i32.sub (local.get $sp) (i32.const 2)) (i32.const 4)))))
                    (i32.store
                      (i32.add (i32.const 1024)
                        (i32.mul (i32.sub (local.get $sp) (i32.const 2)) (i32.const 4)))
                      (i32.add (local.get $a) (local.get $b)))
                    (local.set $sp (i32.sub (local.get $sp) (i32.const 1)))
                    (local.set $pc (i32.add (local.get $pc) (i32.const 1)))
                    (br $loop))
                  ;; MUL: b = pop; a = pop; push a * b
                  (local.set $b (i32.load
                    (i32.add (i32.const 1024)
                      (i32.mul (i32.sub (local.get $sp) (i32.const 1)) (i32.const 4)))))
                  (local.set $a (i32.load
                    (i32.add (i32.const 1024)
                      (i32.mul (i32.sub (local.get $sp) (i32.const 2)) (i32.const 4)))))
                  (i32.store
                    (i32.add (i32.const 1024)
                      (i32.mul (i32.sub (local.get $sp) (i32.const 2)) (i32.const 4)))
                    (i32.mul (local.get $a) (local.get $b)))
                  (local.set $sp (i32.sub (local.get $sp) (i32.const 1)))
                  (local.set $pc (i32.add (local.get $pc) (i32.const 1)))
                  (br $loop))
                ;; HALT: return the value on top of the stack
                (return (i32.load
                  (i32.add (i32.const 1024)
                    (i32.mul (i32.sub (local.get $sp) (i32.const 1)) (i32.const 4))))))
              ;; default: an unknown opcode is a malformed program
              (unreachable))
            (unreachable)))
        "#,
        // func0 = set_instr, func1 = run.
        "let mut inst = Instance::new();\n    \
         let prog = [(0, 3), (0, 4), (1, 0), (0, 5), (2, 0), (3, 0)];\n    \
         for (i, (op, arg)) in prog.iter().enumerate() {\n        \
             inst.func0(i as i32, *op, *arg);\n    \
         }\n    \
         assert_eq!(inst.func1(), 35);",
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
