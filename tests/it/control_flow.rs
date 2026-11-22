//! Integration tests for Phase 2 control flow. Each test transpiles a wasm
//! module, compiles the generated Rust with a real `rustc`, and runs assertions
//! against the produced function so behaviour (not just syntax) is verified.

use crate::common::compile_run;

#[test]
fn if_else_with_result() {
    compile_run(
        "if_else",
        r#"
        (module
          (func (param i32) (result i32)
            (if (result i32) (local.get 0)
              (then (i32.const 10))
              (else (i32.const 20)))))
        "#,
        "assert_eq!(func0(5), 10);\n    assert_eq!(func0(0), 20);",
    );
}

#[test]
fn loop_sums_one_to_n() {
    compile_run(
        "loop_sum",
        r#"
        (module
          (func (param i32) (result i32) (local i32 i32)
            (local.set 1 (i32.const 0))
            (local.set 2 (i32.const 1))
            (block $exit
              (loop $cont
                (br_if $exit (i32.gt_s (local.get 2) (local.get 0)))
                (local.set 1 (i32.add (local.get 1) (local.get 2)))
                (local.set 2 (i32.add (local.get 2) (i32.const 1)))
                (br $cont)))
            (local.get 1)))
        "#,
        "assert_eq!(func0(0), 0);\n    assert_eq!(func0(1), 1);\n    assert_eq!(func0(5), 15);\n    assert_eq!(func0(100), 5050);",
    );
}

#[test]
fn block_with_result_via_br_if() {
    // Returns 1 early when the argument is negative, otherwise 2.
    compile_run(
        "block_result",
        r#"
        (module
          (func (param i32) (result i32)
            (block (result i32)
              (br_if 0 (i32.const 1) (i32.lt_s (local.get 0) (i32.const 0)))
              (drop)
              (i32.const 2))))
        "#,
        "assert_eq!(func0(-3), 1);\n    assert_eq!(func0(7), 2);",
    );
}

#[test]
fn early_return_inside_block() {
    // Returns 222 when the argument is zero, 111 otherwise. Exercises `return`
    // (which makes the rest of the block unreachable) and a void block exit.
    compile_run(
        "early_return",
        r#"
        (module
          (func (param i32) (result i32)
            (block
              (br_if 0 (i32.eqz (local.get 0)))
              (return (i32.const 111)))
            (i32.const 222)))
        "#,
        "assert_eq!(func0(0), 222);\n    assert_eq!(func0(5), 111);",
    );
}

#[test]
fn nested_loops_multiply() {
    // Computes n * m by incrementing an accumulator in a doubly-nested loop,
    // exercising distinct inner/outer loop labels and `continue`/`break`.
    compile_run(
        "nested_loops",
        r#"
        (module
          (func (param i32 i32) (result i32) (local i32 i32 i32)
            (local.set 2 (i32.const 0))
            (local.set 3 (i32.const 0))
            (block $oexit
              (loop $o
                (br_if $oexit (i32.ge_s (local.get 3) (local.get 0)))
                (local.set 4 (i32.const 0))
                (block $iexit
                  (loop $i
                    (br_if $iexit (i32.ge_s (local.get 4) (local.get 1)))
                    (local.set 2 (i32.add (local.get 2) (i32.const 1)))
                    (local.set 4 (i32.add (local.get 4) (i32.const 1)))
                    (br $i)))
                (local.set 3 (i32.add (local.get 3) (i32.const 1)))
                (br $o)))
            (local.get 2)))
        "#,
        "assert_eq!(func0(3, 4), 12);\n    assert_eq!(func0(0, 5), 0);\n    assert_eq!(func0(5, 0), 0);\n    assert_eq!(func0(7, 7), 49);",
    );
}

#[test]
fn br_table_selects_branch() {
    compile_run(
        "br_table",
        r#"
        (module
          (func (param i32) (result i32) (local i32)
            (block $default
              (block $case2
                (block $case1
                  (block $case0
                    (br_table $case0 $case1 $case2 $default (local.get 0)))
                  (local.set 1 (i32.const 100))
                  (br $default))
                (local.set 1 (i32.const 101))
                (br $default))
              (local.set 1 (i32.const 102))
              (br $default))
            (local.get 1)))
        "#,
        "assert_eq!(func0(0), 100);\n    assert_eq!(func0(1), 101);\n    assert_eq!(func0(2), 102);\n    assert_eq!(func0(9), 0);",
    );
}
