//! End-to-end tests that pin the *reason* a transpiled module traps.
//!
//! Other suites already check that certain inputs trap (the process aborts);
//! these assert the panic message too, so a regression that traps for the wrong
//! reason — an out-of-bounds panic where a type-mismatch was expected, say — is
//! caught. Trap reasons come either from the transpiler's own `panic!` strings
//! (`unreachable`, `indirect call type mismatch`, `invalid conversion to
//! integer`, `integer overflow`) or from Rust's built-in arithmetic/indexing
//! panics (`divide by zero`, `divide with overflow`, `index out of bounds`).

use crate::common;

use common::expect_trap_with;

#[test]
fn unreachable_traps_with_its_own_message() {
    expect_trap_with(
        "trap_unreachable",
        r#"(module (func (export "f") (unreachable)))"#,
        "func0();",
        "unreachable",
    );
}

#[test]
fn integer_division_by_zero_traps() {
    expect_trap_with(
        "trap_div0",
        r#"(module (func (export "f") (param i32 i32) (result i32)
             (i32.div_s (local.get 0) (local.get 1))))"#,
        "func0(1, 0);",
        "divide by zero",
    );
}

#[test]
fn i32_signed_division_overflow_traps() {
    expect_trap_with(
        "trap_div_ovf32",
        r#"(module (func (export "f") (param i32 i32) (result i32)
             (i32.div_s (local.get 0) (local.get 1))))"#,
        "func0(i32::MIN, -1);",
        "divide with overflow",
    );
}

#[test]
fn i64_signed_division_overflow_traps() {
    // i64.div_s at iN::MIN / -1 — the 64-bit counterpart is otherwise untested.
    expect_trap_with(
        "trap_div_ovf64",
        r#"(module (func (export "f") (param i64 i64) (result i64)
             (i64.div_s (local.get 0) (local.get 1))))"#,
        "func0(i64::MIN, -1);",
        "divide with overflow",
    );
}

#[test]
fn float_to_int_truncation_of_nan_traps() {
    expect_trap_with(
        "trap_trunc_nan",
        r#"(module (func (export "f") (param f32) (result i32)
             (i32.trunc_f32_s (local.get 0))))"#,
        "func0(f32::NAN);",
        "invalid conversion to integer",
    );
}

#[test]
fn float_to_int_truncation_out_of_range_traps() {
    expect_trap_with(
        "trap_trunc_ovf",
        r#"(module (func (export "f") (param f32) (result i32)
             (i32.trunc_f32_s (local.get 0))))"#,
        "func0(1e30f32);",
        "integer overflow",
    );
}

#[test]
fn call_indirect_through_a_null_slot_traps() {
    // Table slot 1 is never populated (null); dispatching through it traps.
    expect_trap_with(
        "trap_ci_null",
        r#"(module
             (type $sig (func (result i32)))
             (table 2 funcref)
             (elem (i32.const 0) $f)
             (func $f (result i32) (i32.const 42))
             (func (export "call") (param i32) (result i32)
               (call_indirect (type $sig) (local.get 0))))"#,
        "let mut inst = Instance::new();\n    inst.func1(1);",
        "indirect call type mismatch",
    );
}

#[test]
fn call_indirect_with_a_type_mismatch_traps() {
    // Slot 0 holds `$g : (i32) -> i32`, but the call site expects `() -> i32`.
    expect_trap_with(
        "trap_ci_mismatch",
        r#"(module
             (type $sig (func (result i32)))
             (table 1 funcref)
             (elem (i32.const 0) $g)
             (func $g (param i32) (result i32) (local.get 0))
             (func (export "call") (result i32)
               (call_indirect (type $sig) (i32.const 0))))"#,
        "let mut inst = Instance::new();\n    inst.func1();",
        "indirect call type mismatch",
    );
}

#[test]
fn call_indirect_to_an_absent_type_traps() {
    // No defined function has type `$sig = () -> i32` (both funcs are
    // `(i32) -> i32`), and the module has no host. The call can therefore never
    // resolve, so the transpiler emits the trap directly in the body (the
    // `targets.is_empty() && !has_imports` path) rather than a dispatch method —
    // a distinct site from the dispatch-method traps above. The trap must still
    // compile (its cold helper is emitted) and carry the right message.
    expect_trap_with(
        "trap_ci_absent_type",
        r#"(module
             (type $sig (func (result i32)))
             (table 1 funcref)
             (func $only (param i32) (result i32) (local.get 0))
             (func (export "call") (param i32) (result i32)
               (call_indirect (type $sig) (local.get 0))))"#,
        "let mut inst = Instance::new();\n    inst.func1(0);",
        "indirect call type mismatch",
    );
}

#[test]
fn out_of_bounds_memory_load_traps() {
    // One page is 64 KiB; loading at 100000 reaches past it.
    expect_trap_with(
        "trap_load_oob",
        r#"(module
             (memory 1)
             (func (export "f") (result i32) (i32.load (i32.const 100000))))"#,
        "let mut inst = Instance::new();\n    inst.func0();",
        "out of range for slice",
    );
}

// A load is evaluated even when its result is never used: wasm runs every
// instruction, so an out-of-bounds load traps after `drop` and inside the
// operand of `select` that is not chosen.

#[test]
fn a_dropped_out_of_bounds_load_still_traps() {
    expect_trap_with(
        "trap_drop_load",
        r#"(module (memory 1)
             (func (export "f") (i32.load (i32.const 100000000)) drop))"#,
        "let mut inst = Instance::new();\n    inst.func0();",
        "out of range",
    );
}

#[test]
fn an_unchosen_select_operand_that_loads_out_of_bounds_still_traps() {
    for (name, a, b, cond) in [
        (
            "trap_select_load_b",
            "(i32.const 1)",
            "(i32.load (i32.const 100000000))",
            1,
        ),
        (
            "trap_select_load_a",
            "(i32.load (i32.const 100000000))",
            "(i32.const 1)",
            0,
        ),
    ] {
        expect_trap_with(
            name,
            &format!(
                r#"(module (memory 1)
                     (func (export "f") (result i32)
                       (select {a} {b} (i32.const {cond}))))"#
            ),
            "let mut inst = Instance::new();\n    inst.func0();",
            "out of range",
        );
    }
}

#[test]
fn in_bounds_dropped_and_selected_loads_keep_their_values() {
    common::compile_run(
        "drop_select_in_bounds",
        r#"(module (memory 1) (data (i32.const 0) "\07\00\00\00")
             (func (export "d") (i32.load (i32.const 0)) drop)
             (func (export "s") (param i32) (result i32)
               (select (i32.const 9) (i32.load (i32.const 0)) (local.get 0))))"#,
        "let mut inst = Instance::new();\n    inst.func0();\n    assert_eq!(inst.func1(1), 9);\n    assert_eq!(inst.func1(0), 7);",
    );
}

#[test]
fn a_dropped_value_computed_from_an_out_of_bounds_load_still_traps() {
    expect_trap_with(
        "trap_drop_derived_load",
        r#"(module (memory 1)
             (func (export "f")
               (i32.add (i32.load (i32.const 100000000)) (i32.const 1)) drop))"#,
        "let mut inst = Instance::new();\n    inst.func0();",
        "out of range",
    );
}

#[test]
fn a_dropped_out_of_bounds_table_get_still_traps() {
    expect_trap_with(
        "trap_drop_table_get",
        r#"(module (table 1 funcref)
             (func (export "f") (table.get (i32.const 5)) drop))"#,
        "let mut inst = Instance::new();\n    inst.func0();",
        "index out of bounds",
    );
}
