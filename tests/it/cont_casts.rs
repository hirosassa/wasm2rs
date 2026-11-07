//! `ref.test` / `ref.cast` / `br_on_cast` against a concrete continuation type. A
//! continuation handle indexes the instance's `conts` table, which remembers each
//! handle's current declared type: set by `cont.new`, replaced by `cont.bind` and by
//! the continuation type a `resume` handler block gives a suspended continuation. A
//! test then asks whether that type is the target or a declared subtype of it.

use crate::common;

// Funcs: `$body`=0, `$add`=1, `$gen`=2, then the exports from func3. `$cth` is open and
// `$ct` final, so the two are distinct types of the same signature.
const MODULE: &str = r#"(module
  (type $ft (func (result i32)))
  (type $ct (cont $ft))
  (type $cts (sub (cont $ft)))
  (type $ctsub (sub $cts (cont $ft)))
  (type $cth (sub (cont $ft)))
  (type $ft2 (func (param i32) (result i32)))
  (type $ct2 (cont $ft2))
  (type $ft3 (func (param i32 i32) (result i32)))
  (type $ct3 (cont $ft3))
  (tag $yield (param i32))
  (func $body (type $ft) (i32.const 7))
  (func $add (type $ft3) (i32.add (local.get 0) (local.get 1)))
  (func $gen (type $ft) (suspend $yield (i32.const 5)) (i32.const 9))
  (elem declare func $body $add $gen)
  (func (export "same") (result i32)
    (ref.test (ref $ct) (cont.new $ct (ref.func $body))))
  (func (export "unrelated") (result i32)
    (ref.test (ref $ct2) (cont.new $ct (ref.func $body))))
  (func (export "sub_is_super") (result i32)
    (ref.test (ref $cts) (cont.new $ctsub (ref.func $body))))
  (func (export "super_is_not_sub") (result i32)
    (ref.test (ref $ctsub) (cont.new $cts (ref.func $body))))
  (func (export "null_nullable") (result i32) (ref.test (ref null $ct) (ref.null $ct)))
  (func (export "null_non_null") (result i32) (ref.test (ref $ct) (ref.null $ct)))
  (func (export "cast_ok") (result i32)
    (resume $ct (ref.cast (ref $ct) (cont.new $ct (ref.func $body)))))
  (func (export "cast_mismatch") (result i32)
    (drop (ref.cast (ref $ct2) (cont.new $ct (ref.func $body))))
    (i32.const 0))
  (func (export "br_on_cast_hit") (result i32)
    (block $b (result (ref $ct))
      (br_on_cast $b (ref cont) (ref $ct) (cont.new $ct (ref.func $body)))
      drop
      (return (i32.const 0)))
    drop
    i32.const 1)
  (func (export "br_on_cast_fail_taken") (result i32)
    (block $b (result (ref cont))
      (br_on_cast_fail $b (ref cont) (ref $ct2) (cont.new $ct (ref.func $body)))
      drop
      (return (i32.const 1)))
    drop
    i32.const 0)
  (func (export "bound") (result i32) (local $k (ref null $ct2))
    (local.set $k (cont.bind $ct3 $ct2 (i32.const 10) (cont.new $ct3 (ref.func $add))))
    (i32.add
      (i32.mul (ref.test (ref $ct2) (local.get $k)) (i32.const 10))
      (ref.test (ref $ct3) (local.get $k))))
  (func (export "suspended") (result i32) (local $k (ref null $cth))
    (block $on_yield (result i32 (ref $cth))
      (resume $ct (on $yield $on_yield) (cont.new $ct (ref.func $gen)))
      return)
    local.set $k
    drop
    (i32.add
      (i32.mul (ref.test (ref $cth) (local.get $k)) (i32.const 10))
      (ref.test (ref $ct) (local.get $k)))))"#;

#[test]
fn ref_test_on_a_concrete_continuation_type_follows_declared_subtyping() {
    common::compile_run(
        "cont_cast_test",
        MODULE,
        "let mut inst = Instance::new();\n assert_eq!(inst.func3(), 1);\n assert_eq!(inst.func4(), 0);\n \
         assert_eq!(inst.func5(), 1);\n assert_eq!(inst.func6(), 0);\n \
         assert_eq!(inst.func7(), 1);\n assert_eq!(inst.func8(), 0);",
    );
}

#[test]
fn ref_cast_to_a_concrete_continuation_type_passes_a_match_and_traps_otherwise() {
    common::compile_run(
        "cont_cast_cast",
        MODULE,
        "let mut inst = Instance::new();\n assert_eq!(inst.func9(), 7);\n \
         let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| inst.func10()));\n assert!(r.is_err());",
    );
}

#[test]
fn br_on_cast_to_a_concrete_continuation_type_branches_on_a_match() {
    common::compile_run(
        "cont_cast_br",
        MODULE,
        "let mut inst = Instance::new();\n assert_eq!(inst.func11(), 1);\n assert_eq!(inst.func12(), 0);",
    );
}

#[test]
fn bind_and_a_resume_handler_retype_the_continuation() {
    // `cont.bind` makes it a `$ct2` (no longer `$ct3`); a continuation suspended
    // into a handler block typed `$cth` is a `$cth`, not the `$ct` it was created as.
    common::compile_run(
        "cont_cast_retype",
        MODULE,
        "let mut inst = Instance::new();\n assert_eq!(inst.func13(), 10);\n assert_eq!(inst.func14(), 10);",
    );
}

#[test]
fn a_module_that_creates_no_continuation_has_only_null_to_test() {
    common::compile_run(
        "cont_cast_none",
        r#"(module
          (type $ft (func))
          (type $ct (cont $ft))
          (func (export "null_nullable") (result i32) (ref.test (ref null $ct) (ref.null cont)))
          (func (export "null_non_null") (result i32) (ref.test (ref $ct) (ref.null cont))))"#,
        "assert_eq!(func0(), 1); assert_eq!(func1(), 0);",
    );
}

#[test]
fn a_reference_kept_across_cont_bind_still_has_its_own_type() {
    // `cont.bind` consumes `$k` (a `$ct3`) into a new `$ct2`; the old reference is
    // still a `$ct3`, and is no longer resumable.
    common::compile_run(
        "cont_cast_old_bind",
        r#"(module
          (type $ft3 (func (param i32 i32) (result i32)))
          (type $ct3 (cont $ft3))
          (type $ft2 (func (param i32) (result i32)))
          (type $ct2 (cont $ft2))
          (func $add (type $ft3) (i32.add (local.get 0) (local.get 1)))
          (elem declare func $add)
          (func (export "run") (result i32) (local $k (ref null $ct3))
            (local.set $k (cont.new $ct3 (ref.func $add)))
            (drop (cont.bind $ct3 $ct2 (i32.const 10) (local.get $k)))
            (ref.test (ref $ct3) (local.get $k))))"#,
        "let mut inst = Instance::new(); assert_eq!(inst.func1(), 1);",
    );
}

#[test]
fn a_reference_kept_across_a_suspend_still_has_its_own_type() {
    common::compile_run(
        "cont_cast_old_suspend",
        r#"(module
          (type $ft (func (result i32)))
          (type $ct (cont $ft))
          (type $cth (sub (cont $ft)))
          (tag $yield (param i32))
          (func $gen (type $ft) (suspend $yield (i32.const 5)) (i32.const 9))
          (elem declare func $gen)
          (func (export "run") (result i32) (local $k1 (ref null $ct))
            (local.set $k1 (cont.new $ct (ref.func $gen)))
            (block $on_yield (result i32 (ref $cth))
              (resume $ct (on $yield $on_yield) (local.get $k1))
              return)
            drop drop
            (ref.test (ref $ct) (local.get $k1))))"#,
        "let mut inst = Instance::new(); assert_eq!(inst.func1(), 1);",
    );
}
