//! A trapping operand that a branch discards must still trap: the branch
//! cannot drop an operand whose evaluation may trap, such as an out-of-bounds
//! load, from the generated code.

use crate::common;

const MEM: &str = "(memory 1)";

#[test]
fn a_trapping_load_before_a_function_level_br_traps() {
    common::expect_trap(
        "tbb_br0",
        &format!(
            r#"(module {MEM} (func (export "f")
                 (i32.load (i32.const 1000000)) (br 0)))"#
        ),
        "let mut inst = Instance::new();\n inst.func0();",
    );
}

#[test]
fn a_trapping_load_before_a_br_out_of_a_block_traps() {
    common::expect_trap(
        "tbb_br1",
        &format!(
            r#"(module {MEM} (func (export "f")
                 (block (i32.load (i32.const 1000000)) (br 1))))"#
        ),
        "let mut inst = Instance::new();\n inst.func0();",
    );
}

#[test]
fn a_trapping_load_before_a_taken_br_if_traps() {
    common::expect_trap(
        "tbb_br_if",
        &format!(
            r#"(module {MEM} (func (export "f") (param i32)
                 (block (i32.load (i32.const 1000000)) (local.get 0) (br_if 0) (drop))))"#
        ),
        "let mut inst = Instance::new();\n inst.func0(1);",
    );
}

#[test]
fn a_trapping_load_before_a_return_traps() {
    common::expect_trap(
        "tbb_return",
        &format!(
            r#"(module {MEM} (func (export "f")
                 (i32.load (i32.const 1000000)) (return)))"#
        ),
        "let mut inst = Instance::new();\n inst.func0();",
    );
}

#[test]
fn a_trapping_operand_below_a_branch_value_still_traps() {
    common::expect_trap(
        "tbb_value",
        &format!(
            r#"(module {MEM} (func (export "f") (result i32)
                 (i32.load (i32.const 1000000)) (i32.const 7) (br 0)))"#
        ),
        "let mut inst = Instance::new();\n inst.func0();",
    );
}

#[test]
fn an_in_bounds_load_before_a_br_does_not_trap() {
    common::compile_run(
        "tbb_ok",
        &format!(
            r#"(module {MEM} (func (export "f") (result i32)
                 (i32.load (i32.const 8)) (i32.const 7) (br 0)))"#
        ),
        "let mut inst = Instance::new();\n assert_eq!(inst.func0(), 7);",
    );
}
