//! Integration tests for Phase 6f: multi-value function results. A function
//! returning N values maps to a Rust tuple `(T0, .., Tn)`; callers destructure
//! it back onto the operand stack. Each module is compiled with
//! `rustc -D warnings` and exercised.

use crate::common::compile_run;

#[test]
fn function_returns_a_pair() {
    // Swap: pushes arg1 then arg0, so the result tuple is (l1, l0).
    compile_run(
        "pair",
        r#"
        (module
          (func (param i32 i32) (result i32 i32)
            (local.get 1) (local.get 0)))
        "#,
        "assert_eq!(func0(3, 7), (7, 3));",
    );
}

#[test]
fn function_returns_three_mixed_values() {
    compile_run(
        "triple",
        r#"
        (module
          (func (result i32 i64 f32)
            (i32.const 1) (i64.const 2) (f32.const 3.5)))
        "#,
        "assert_eq!(func0(), (1, 2i64, 3.5f32));",
    );
}

#[test]
fn call_destructures_multi_value_result() {
    // func0 returns (x, 2x); func1 calls it and adds the two results: 3x.
    compile_run(
        "call",
        r#"
        (module
          (func (param i32) (result i32 i32)
            (local.get 0) (i32.add (local.get 0) (local.get 0)))
          (func (param i32) (result i32)
            (call 0 (local.get 0))
            (i32.add)))
        "#,
        "assert_eq!(func1(5), 15);",
    );
}

#[test]
fn call_indirect_destructures_multi_value_result() {
    // Table slot 0 is func0 returning (x, x*x); func1 dispatches and adds them.
    compile_run(
        "indirect",
        r#"
        (module
          (type $t (func (param i32) (result i32 i32)))
          (table 1 funcref)
          (elem (i32.const 0) 0)
          (func (param i32) (result i32 i32)
            (local.get 0) (i32.mul (local.get 0) (local.get 0)))
          (func (param i32) (result i32)
            (call_indirect (type $t) (local.get 0) (i32.const 0))
            (i32.add)))
        "#,
        "let mut inst = Instance::new();\n    assert_eq!(inst.func1(4), 20);",
    );
}
