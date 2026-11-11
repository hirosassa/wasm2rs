//! `exnref` values: the `_ref` forms of `try_table` catch clauses capture the
//! caught exception as a handle, and `throw_ref` raises it again.

use crate::common::{compile_run, expect_trap};

#[test]
fn a_null_exnref_is_null() {
    compile_run(
        "exn_null",
        r#"(module
            (func (export "f") (result i32)
              (ref.is_null (ref.null exn))))"#,
        "assert_eq!(func0(), 1);",
    );
}

#[test]
fn an_exnref_local_defaults_to_null_and_passes_through_a_call() {
    compile_run(
        "exn_local",
        r#"(module
            (func $id (param exnref) (result exnref) (local.get 0))
            (func (export "f") (result i32)
              (local exnref)
              (i32.add
                (ref.is_null (local.get 0))
                (ref.is_null (call $id (local.get 0))))))"#,
        "assert_eq!(func1(), 2);",
    );
}

#[test]
fn catch_all_ref_captures_the_exception_and_throw_ref_rethrows_it() {
    compile_run(
        "exn_rethrow",
        r#"(module
            (tag $e (param i32))
            (func (export "f") (param i32) (result i32)
              (block $outer (result i32)
                (try_table (result i32) (catch $e $outer)
                  (throw_ref
                    (block $h (result exnref)
                      (try_table (catch_all_ref $h) (throw $e (local.get 0)))
                      (unreachable)))))))"#,
        "assert_eq!(func0(9), 9); assert_eq!(func0(-3), -3);",
    );
}

#[test]
fn catch_ref_pushes_the_payload_then_the_exnref() {
    compile_run(
        "exn_catch_ref",
        r#"(module
            (tag $e (param i32))
            (func (export "f") (param i32) (result i32)
              (local $x exnref) (local $p i32)
              (block $h (result i32 exnref)
                (try_table (catch_ref $e $h) (throw $e (local.get 0)))
                (unreachable))
              (local.set $x)
              (local.set $p)
              (i32.add (local.get $p) (i32.add (i32.const 1) (ref.is_null (local.get $x))))))"#,
        "assert_eq!(func0(4), 5);",
    );
}

#[test]
fn a_caught_exnref_can_be_rethrown_more_than_once() {
    compile_run(
        "exn_twice",
        r#"(module
            (tag $e (param i32))
            (func $catch_it (param i32) (result exnref)
              (block $h (result exnref)
                (try_table (catch_all_ref $h) (throw $e (local.get 0)))
                (unreachable)))
            (func $caught (param exnref) (result i32)
              (block $o (result i32)
                (try_table (result i32) (catch $e $o)
                  (throw_ref (local.get 0)))))
            (func (export "f") (param i32) (result i32)
              (local $x exnref)
              (local.set $x (call $catch_it (local.get 0)))
              (i32.add (call $caught (local.get $x)) (call $caught (local.get $x)))))"#,
        "assert_eq!(func2(5), 10);",
    );
}

#[test]
fn throw_ref_of_a_null_reference_traps() {
    expect_trap(
        "exn_throw_null",
        r#"(module
            (func (export "f") (throw_ref (ref.null exn))))"#,
        "func0();",
    );
}

#[test]
fn throw_ref_is_caught_by_a_legacy_catch_all() {
    compile_run(
        "exn_legacy",
        r#"(module
            (tag $e)
            (func (export "f") (result i32)
              (local $x exnref)
              (local.set $x
                (block $h (result exnref)
                  (try_table (catch_all_ref $h) (throw $e))
                  (unreachable)))
              try (result i32)
                (throw_ref (local.get $x))
              catch_all
                (i32.const 7)
              end))"#,
        "assert_eq!(func0(), 7);",
    );
}

#[test]
fn an_exnref_global_holds_a_caught_exception() {
    compile_run(
        "exn_global",
        r#"(module
            (tag $e (param i32))
            (global $g (mut exnref) (ref.null exn))
            (func (export "f") (param i32) (result i32)
              (local $before i32)
              (local.set $before (ref.is_null (global.get $g)))
              (global.set $g
                (block $h (result exnref)
                  (try_table (catch_all_ref $h) (throw $e (local.get 0)))
                  (unreachable)))
              (i32.add (local.get $before)
                (i32.mul (i32.const 10) (ref.is_null (global.get $g))))))"#,
        "let mut i = Instance::new(); assert_eq!(i.func0(3), 1);",
    );
}

const TABLE_MODULE: &str = r#"(module
    (tag $e (param i32))
    (table $t 2 exnref)
    (func $mk (param i32) (result exnref)
      (block $h (result exnref)
        (try_table (catch_all_ref $h) (throw $e (local.get 0)))
        (unreachable)))
    (func (export "sizes") (result i32)
      (i32.add
        (ref.is_null (table.get $t (i32.const 0)))
        (i32.add
          (i32.mul (i32.const 10) (ref.is_null (table.get $t (i32.const 1))))
          (i32.mul (i32.const 100) (table.size $t)))))
    (func (export "rethrow") (param i32) (result i32)
      (table.set $t (i32.const 0) (call $mk (local.get 0)))
      (block $o (result i32)
        (try_table (result i32) (catch $e $o)
          (throw_ref (table.get $t (i32.const 0))))))
    (func (export "grow") (param i32) (result i32)
      (drop (table.grow $t (call $mk (local.get 0)) (i32.const 2)))
      (i32.add (table.size $t) (ref.is_null (table.get $t (i32.const 3)))))
    (func (export "fill_copy") (param i32) (result i32)
      (table.fill $t (i32.const 0) (call $mk (local.get 0)) (i32.const 1))
      (table.copy $t $t (i32.const 1) (i32.const 0) (i32.const 1))
      (ref.is_null (table.get $t (i32.const 1)))))
    "#;

#[test]
fn an_exnref_table_starts_null_and_reports_its_size() {
    compile_run(
        "exn_table_size",
        TABLE_MODULE,
        "let mut i = Instance::new(); assert_eq!(i.func1(), 211);",
    );
}

#[test]
fn an_exnref_table_slot_holds_a_rethrowable_exception() {
    compile_run(
        "exn_table_rethrow",
        TABLE_MODULE,
        "let mut i = Instance::new(); assert_eq!(i.func2(7), 7);",
    );
}

#[test]
fn an_exnref_table_grows_fills_and_copies() {
    compile_run(
        "exn_table_grow",
        TABLE_MODULE,
        "let mut i = Instance::new(); assert_eq!(i.func3(1), 4); \
         let mut j = Instance::new(); assert_eq!(j.func4(1), 0);",
    );
}

#[test]
fn an_exnref_elem_segment_may_hold_null_items() {
    compile_run(
        "exn_table_elem",
        r#"(module
            (table $t 2 exnref)
            (elem (i32.const 0) exnref (ref.null exn) (ref.null exn))
            (func (export "f") (result i32)
              (i32.add (ref.is_null (table.get $t (i32.const 0)))
                       (ref.is_null (table.get $t (i32.const 1))))))"#,
        "let mut i = Instance::new(); assert_eq!(i.func0(), 2);",
    );
}

#[test]
fn a_funcref_and_an_exnref_ride_in_an_exception_payload() {
    compile_run(
        "exn_payload_refs",
        r#"(module
            (tag $e (param funcref exnref))
            (tag $d)
            (func $f)
            (elem declare func $f)
            (func $mk (result exnref)
              (block $h (result exnref)
                (try_table (catch_all_ref $h) (throw $d))
                (unreachable)))
            (func (export "f") (result funcref)
              (block $h (result funcref exnref)
                (try_table (result funcref exnref) (catch $e $h)
                  (throw $e (ref.func $f) (call $mk))))
              (drop)))"#,
        "assert_eq!(func2(), 0);",
    );
}

#[test]
fn an_externref_and_a_typed_funcref_ride_in_an_exception_payload() {
    compile_run(
        "exn_payload_typed",
        r#"(module
            (type $ft (func))
            (tag $e (param externref (ref null $ft)))
            (func $f (type $ft))
            (elem declare func $f)
            (func (export "f") (result (ref null $ft))
              (local $r (ref null $ft)) (local $x externref)
              (block $h (result externref (ref null $ft))
                (try_table (result externref (ref null $ft)) (catch $e $h)
                  (throw $e (ref.null extern) (ref.func $f))))
              (local.set $r)
              (local.set $x)
              (local.get $r)))"#,
        "assert_eq!(func1(), 0);",
    );
}

#[test]
fn a_caught_exnref_payload_can_be_thrown_again() {
    compile_run(
        "exn_payload_exnref",
        r#"(module
            (tag $d (param i32))
            (tag $wrap (param exnref))
            (func (export "f") (param i32) (result i32)
              (block $o (result i32)
                (try_table (result i32) (catch $d $o)
                  (throw_ref
                    (block $w (result exnref)
                      (try_table (catch $wrap $w)
                        (throw $wrap
                          (block $h (result exnref)
                            (try_table (catch_all_ref $h) (throw $d (local.get 0)))
                            (unreachable))))
                      (unreachable)))))))"#,
        "assert_eq!(func0(11), 11);",
    );
}

// A caught exnref that is dropped must release its exception: repeating
// `catch_ref` in a long loop may not grow the live heap. The counting allocator
// measures that independently of how the generated code stores exceptions.
#[test]
fn repeated_catch_ref_does_not_grow_the_heap() {
    compile_run(
        "exn_no_leak",
        r#"(module
            (tag $e (param i32))
            (func (export "f") (param i32)
              (local $i i32)
              (loop $l
                (drop
                  (block $h (result i32 exnref)
                    (try_table (catch_ref $e $h) (throw $e (local.get $i)))
                    (unreachable)))
                (drop)
                (local.set $i (i32.add (local.get $i) (i32.const 1)))
                (br_if $l (i32.lt_u (local.get $i) (local.get 0))))))"#,
        r#"
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
struct Counting;
static LIVE: AtomicUsize = AtomicUsize::new(0);
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 { LIVE.fetch_add(l.size(), Ordering::Relaxed); unsafe { System.alloc(l) } }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) { LIVE.fetch_sub(l.size(), Ordering::Relaxed); unsafe { System.dealloc(p, l) } }
}
#[global_allocator]
static A: Counting = Counting;
func0(10);
let before = LIVE.load(Ordering::Relaxed);
func0(200_000);
let grown = LIVE.load(Ordering::Relaxed).saturating_sub(before);
assert!(grown < 64 * 1024, "heap grew by {grown} bytes");
"#,
    );
}

const EXN_PRELUDE: &str = r#"(tag $e (param i32)) (tag $w (param exnref))
  (func $mk (param i32) (result exnref)
    (block $h (result exnref) (try_table (catch_all_ref $h) (throw $e (local.get 0))) (unreachable)))
  (func $rt (param exnref) (result i32)
    (block $o (result i32) (try_table (result i32) (catch $e $o) (throw_ref (local.get 0)))))"#;

fn exn_module(body: &str) -> String {
    format!("(module (table $t 2 exnref) {EXN_PRELUDE} {body})")
}

// A caught exnref is not `Copy`: using it as a throw payload must not move it
// out of the local that later code reads again.
#[test]
fn an_exnref_local_can_be_thrown_as_a_payload_and_used_again() {
    compile_run(
        "exn_throw_local",
        &exn_module(
            r#"(func (export "f") (param i32) (result i32) (local $x exnref)
              (local.set $x (call $mk (local.get 0)))
              (block $c (try_table (catch_all $c) (throw $w (local.get $x))))
              (call $rt (local.get $x)))"#,
        ),
        "let mut i = Instance::new(); assert_eq!(i.func2(3), 3);",
    );
}

#[test]
fn ref_test_and_ref_cast_work_on_an_exnref() {
    compile_run(
        "exn_ref_test",
        &exn_module(
            r#"(func (export "f") (param i32) (result i32) (local $x exnref)
              (local.set $x (call $mk (local.get 0)))
              (i32.add (ref.test (ref exn) (local.get $x))
                (i32.add (i32.mul (i32.const 10) (ref.test (ref exn) (ref.null exn)))
                  (call $rt (ref.cast (ref exn) (local.get $x))))))"#,
        ),
        "let mut i = Instance::new(); assert_eq!(i.func2(5), 6);",
    );
}

// Spilling a still-live (mutable) exnref operand must clone it, not move it.
#[test]
fn a_spilled_mutable_exnref_local_stays_usable() {
    compile_run(
        "exn_spill",
        &exn_module(
            r#"(func (export "f") (param i32) (result i32) (local $x exnref) (local $y exnref)
              (local.set $x (call $mk (local.get 0)))
              (local.get $x)
              (local.set $y (local.get $x))
              (local.set $x (ref.null exn))
              (call $rt)
              (call $rt (local.get $y)) (i32.add))
            (func (export "g") (param i32) (result i32) (local $x exnref)
              (local.set $x (call $mk (local.get 0)))
              (table.set $t (i32.const 0) (local.get $x))
              (i32.add (call $rt (local.get $x)) (call $rt (table.get $t (i32.const 0)))))"#,
        ),
        "let mut i = Instance::new(); assert_eq!(i.func2(3), 6); assert_eq!(i.func3(3), 6);",
    );
}
