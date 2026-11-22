//! Integration tests for Phase 4a: direct calls (`call`). Each test transpiles
//! a module, compiles the generated Rust with `rustc -D warnings`, and runs
//! assertions so the call behaviour (arguments, results, recursion, and
//! free-fn vs `&mut self` method dispatch) is verified end to end.

use crate::common::compile_run;

#[test]
fn direct_call_passes_arguments_and_result() {
    compile_run(
        "direct",
        r#"
        (module
          (func (param i32 i32) (result i32) (i32.add (local.get 0) (local.get 1)))
          (func (param i32) (result i32) (call 0 (local.get 0) (i32.const 10))))
        "#,
        "assert_eq!(func1(5), 15);\n    assert_eq!(func1(-4), 6);",
    );
}

#[test]
fn recursive_call_sums_to_n() {
    compile_run(
        "recursion",
        r#"
        (module
          (func (param i32) (result i32)
            (if (result i32) (i32.eqz (local.get 0))
              (then (i32.const 0))
              (else (i32.add (local.get 0)
                             (call 0 (i32.sub (local.get 0) (i32.const 1))))))))
        "#,
        "assert_eq!(func0(5), 15);\n    assert_eq!(func0(0), 0);",
    );
}

#[test]
fn mutual_recursion_parity() {
    compile_run(
        "mutual",
        r#"
        (module
          (func (param i32) (result i32)
            (if (result i32) (i32.eqz (local.get 0))
              (then (i32.const 1))
              (else (call 1 (i32.sub (local.get 0) (i32.const 1))))))
          (func (param i32) (result i32)
            (if (result i32) (i32.eqz (local.get 0))
              (then (i32.const 0))
              (else (call 0 (i32.sub (local.get 0) (i32.const 1)))))))
        "#,
        "assert_eq!(func0(10), 1);\n    \
         assert_eq!(func1(10), 0);\n    \
         assert_eq!(func0(7), 0);\n    \
         assert_eq!(func1(7), 1);",
    );
}

#[test]
fn operand_keeps_pre_call_value_when_callee_mutates_global() {
    // The left operand of the `add` reads global 0 *before* the call, and the
    // callee overwrites that global. Correct spill-before-call must freeze the
    // pre-call value (100), so the result is 100 + (100 + 1) = 201, not 101.
    compile_run(
        "spill",
        r#"
        (module
          (global (mut i32) (i32.const 100))
          (func (param i32) (result i32)
            (global.set 0 (i32.const 0))
            (i32.add (local.get 0) (i32.const 1)))
          (func (result i32)
            (i32.add (global.get 0) (call 0 (global.get 0)))))
        "#,
        "let mut inst = Instance::new();\n    assert_eq!(inst.func1(), 201);",
    );
}

#[test]
fn void_call_in_stateful_module_dispatches_on_self() {
    // A module with a global becomes a `struct Instance`; the caller must
    // reach the callee through `self`, and the void call runs for its effect.
    compile_run(
        "stateful",
        r#"
        (module
          (global (mut i32) (i32.const 0))
          (func (param i32)
            (global.set 0 (i32.add (global.get 0) (local.get 0))))
          (func (param i32) (result i32)
            (call 0 (local.get 0))
            (global.get 0)))
        "#,
        "let mut inst = Instance::new();\n    \
         assert_eq!(inst.func1(5), 5);\n    \
         assert_eq!(inst.func1(3), 8);",
    );
}

#[test]
fn a_directly_recursive_function_compiles_without_warnings() {
    // `runaway` calls itself unconditionally (it would exhaust the stack if it
    // were ever called). rustc's `unconditional_recursion` lint must not make
    // the generated code fail `-D warnings`.
    compile_run(
        "calls_unconditional_recursion",
        r#"(module
          (func $runaway (export "runaway") (call $runaway))
          (func (export "one") (result i32) i32.const 1))"#,
        "assert_eq!(func1(), 1);",
    );
}
