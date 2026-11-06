//! `ref.test` / `ref.cast` / `br_on_cast` against a concrete function type. A
//! funcref is a function-index handle, so (as for `call_indirect`) the runtime
//! type of a non-null funcref is its function's signature: it matches a target
//! function type exactly when the signatures are equal.

use crate::common;

const MODULE: &str = r#"(module
  (type $unary (func (param i32) (result i32)))
  (type $nullary (func))
  (func $inc (type $unary) (i32.add (local.get 0) (i32.const 1)))
  (func $nop (type $nullary))
  (elem declare func $inc $nop)
  (func (export "inc_is_unary") (result i32) (ref.test (ref $unary) (ref.func $inc)))
  (func (export "inc_is_nullary") (result i32) (ref.test (ref $nullary) (ref.func $inc)))
  (func (export "null_non_null_target") (result i32) (ref.test (ref $unary) (ref.null func)))
  (func (export "null_nullable_target") (result i32) (ref.test (ref null $unary) (ref.null func)))
  (func (export "cast_ok") (result i32)
    (call_ref $unary (i32.const 41) (ref.cast (ref $unary) (ref.func $inc))))
  (func (export "cast_mismatch") (result i32)
    (call_ref $unary (i32.const 41) (ref.cast (ref $unary) (ref.func $nop))))
  (func (export "br_on_cast_hit") (result i32)
    (block $b (result (ref $unary))
      (br_on_cast $b funcref (ref $unary) (ref.func $inc))
      drop
      (return (i32.const 0)))
    drop
    i32.const 1)
  (func (export "br_on_cast_fail_taken") (result i32)
    (block $b (result funcref)
      (br_on_cast_fail $b funcref (ref $unary) (ref.func $nop))
      drop
      (return (i32.const 1)))
    drop
    i32.const 0))"#;

#[test]
fn ref_test_on_a_concrete_function_type_compares_signatures() {
    // Funcs: `$inc`=0, `$nop`=1, then the exports from func2.
    common::compile_run(
        "func_cast_test",
        MODULE,
        "let mut inst = Instance::new();\n assert_eq!(inst.func2(), 1);\n assert_eq!(inst.func3(), 0);\n \
         assert_eq!(inst.func4(), 0);\n assert_eq!(inst.func5(), 1);",
    );
}

#[test]
fn ref_cast_to_a_concrete_function_type_passes_a_match_and_traps_otherwise() {
    common::compile_run(
        "func_cast_cast",
        MODULE,
        "let mut inst = Instance::new();\n assert_eq!(inst.func6(), 42);\n \
         let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| inst.func7()));\n assert!(r.is_err());",
    );
}

#[test]
fn br_on_cast_to_a_concrete_function_type_branches_on_a_match() {
    common::compile_run(
        "func_cast_br",
        MODULE,
        "let mut inst = Instance::new();\n assert_eq!(inst.func8(), 1);\n assert_eq!(inst.func9(), 0);",
    );
}
