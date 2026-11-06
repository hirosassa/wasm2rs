//! `ref.test` / `ref.cast` / `br_on_cast` against a concrete function type. A
//! funcref is a function-index handle, so (as for `call_indirect`) the runtime
//! type of a non-null funcref is its function's signature: it matches a target
//! function type exactly when the signatures are equal.

use crate::common;

// Funcs: `$inc`=0, `$nop`=1, then the exports from func2.
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

// Declared subtyping and canonical equivalence decide the test, not signature
// equality. Funcs: `$h`=0, `$x`=1, `$y`=2, `$k`=3, `$m`=4, then the exports.
const SUBTYPING: &str = r#"(module
  (type $f (sub (func (result funcref))))
  (type $g (sub $f (func (result (ref func)))))
  (type $gg (sub $g (func (result (ref func)))))
  (type $a (sub (func)))
  (type $b (func))
  (type $s1 (struct)) (type $s2 (struct))
  (type $p1 (func (param (ref null $s1))))
  (type $p2 (func (param (ref null $s2))))
  (rec (type $r1 (func)) (type $r2 (func (param i32))))
  (rec (type $q1 (func)) (type $q2 (func (param i32))))
  (func $h (type $gg) (ref.func $h))
  (func $x (type $a))
  (func $y (type $p1))
  (func $k (type $r1))
  (func $m (type $q2))
  (elem declare func $h $x $y $k $m)
  (func (export "sub_of_declared_super") (result i32) (ref.test (ref $f) (ref.func $h)))
  (func (export "two_levels_down") (result i32) (ref.test (ref $g) (ref.func $h)))
  (func (export "super_is_not_sub") (result i32) (ref.test (ref $gg) (ref.func $h)))
  (func (export "final_vs_nonfinal") (result i32) (ref.test (ref $b) (ref.func $x)))
  (func (export "same_sig_nonfinal") (result i32) (ref.test (ref $a) (ref.func $x)))
  (func (export "equivalent_params") (result i32) (ref.test (ref $p2) (ref.func $y)))
  (func (export "same_rec_group_shape") (result i32) (ref.test (ref $r1) (ref.func $k)))
  (func (export "equivalent_rec_groups") (result i32) (ref.test (ref $r2) (ref.func $m)))
  (func (export "cross_rec_member") (result i32) (ref.test (ref $r1) (ref.func $m))))"#;

#[test]
fn ref_test_follows_declared_function_subtyping() {
    common::compile_run(
        "func_sub_decl",
        SUBTYPING,
        "assert_eq!(func5(), 1);\n \
         assert_eq!(func6(), 1);\n assert_eq!(func7(), 1);\n",
    );
}

#[test]
fn ref_test_distinguishes_final_and_open_function_types_of_one_signature() {
    common::compile_run(
        "func_sub_final",
        SUBTYPING,
        "assert_eq!(func8(), 0);\n \
         assert_eq!(func9(), 1);\n",
    );
}

#[test]
fn ref_test_treats_equivalent_function_types_as_one() {
    common::compile_run(
        "func_sub_equiv",
        SUBTYPING,
        "assert_eq!(func10(), 1);\n \
         assert_eq!(func11(), 1);\n assert_eq!(func12(), 1);\n \
         assert_eq!(func13(), 0);\n",
    );
}
