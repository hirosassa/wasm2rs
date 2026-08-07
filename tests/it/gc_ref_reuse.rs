//! End-to-end tests pinning that a managed (`GcRef`) reference read from a local
//! stays usable after it is *also* consumed by-value elsewhere. A `GcRef` is not
//! `Copy`, so every by-value consumer must clone the handle rather than move it;
//! otherwise the generated Rust fails to compile (use-after-move). These probe
//! the consumers beyond call arguments / aggregate slots / continuation frames
//! (which have their own coverage): `local.set`/`local.tee`, `global.set`, a
//! multi-value `return`, block/branch result carrying, and `select`.

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
fn local_set_copies_a_reference_and_the_source_survives() {
    // `local.set $q (local.get $r)` copies the handle into another local; the
    // source `$r` must remain usable. Both locals name the same object, so both
    // field reads see `n`, summing to `2n`.
    compile_run(
        "gc_reuse_local_set",
        r#"(module
            (type $s (struct (field i32)))
            (func (export "f") (param i32) (result i32)
              (local $r (ref $s)) (local $q (ref $s))
              (local.set $r (struct.new $s (local.get 0)))
              (local.set $q (local.get $r))
              (i32.add
                (struct.get $s 0 (local.get $r))
                (struct.get $s 0 (local.get $q)))))"#,
        "assert_eq!(func0(21), 42); assert_eq!(func0(0), 0);",
    );
}

#[test]
fn local_tee_leaves_the_reference_on_the_stack_and_in_the_local() {
    // `local.tee $q` both stores the handle into `$q` and leaves it on the stack
    // for the enclosing `struct.get`. The teed value and the later `$q` read name
    // the same object.
    compile_run(
        "gc_reuse_local_tee",
        r#"(module
            (type $s (struct (field i32)))
            (func (export "f") (param i32) (result i32)
              (local $r (ref $s)) (local $q (ref $s))
              (local.set $r (struct.new $s (local.get 0)))
              (i32.add
                (struct.get $s 0 (local.tee $q (local.get $r)))
                (struct.get $s 0 (local.get $q)))))"#,
        "assert_eq!(func0(21), 42);",
    );
}

#[test]
fn global_set_stores_a_reference_and_the_source_survives() {
    // `global.set $g (local.get $r)` moves the handle into a global; the local
    // `$r` must still be readable afterwards. Both name the same object.
    compile_run(
        "gc_reuse_global_set",
        r#"(module
            (type $s (struct (field i32)))
            (global $g (mut (ref null $s)) (ref.null $s))
            (func (export "f") (param i32) (result i32)
              (local $r (ref $s))
              (local.set $r (struct.new $s (local.get 0)))
              (global.set $g (local.get $r))
              (i32.add
                (struct.get $s 0 (local.get $r))
                (struct.get $s 0 (global.get $g)))))"#,
        "let mut inst = Instance::new(); \
         assert_eq!(inst.func0(21), 42);",
    );
}

#[test]
fn block_result_carries_two_copies_of_one_reference() {
    // A `block` yielding `(ref $s) (ref $s)` from the same local pushes the
    // handle twice; the block-result assignment must clone, not move.
    compile_run(
        "gc_reuse_block_result",
        r#"(module
            (type $s (struct (field i32)))
            (func (export "f") (param i32) (result i32)
              (local $r (ref $s)) (local $x (ref $s)) (local $y (ref $s))
              (local.set $r (struct.new $s (local.get 0)))
              (block (result (ref $s) (ref $s))
                (local.get $r) (local.get $r))
              (local.set $y)
              (local.set $x)
              (i32.add
                (struct.get $s 0 (local.get $x))
                (struct.get $s 0 (local.get $y)))))"#,
        "assert_eq!(func0(21), 42);",
    );
}

#[test]
fn branch_carries_two_copies_of_one_reference() {
    // A `br` out of a block carries `(ref $s) (ref $s)` built from one local; the
    // branch's value assignment must clone each copy rather than move.
    compile_run(
        "gc_reuse_branch",
        r#"(module
            (type $s (struct (field i32)))
            (func (export "f") (param i32) (result i32)
              (local $r (ref $s)) (local $x (ref $s)) (local $y (ref $s))
              (local.set $r (struct.new $s (local.get 0)))
              (block $b (result (ref $s) (ref $s))
                (local.get $r) (local.get $r)
                (br $b))
              (local.set $y)
              (local.set $x)
              (i32.add
                (struct.get $s 0 (local.get $x))
                (struct.get $s 0 (local.get $y)))))"#,
        "assert_eq!(func0(21), 42);",
    );
}

#[test]
fn select_arms_do_not_consume_a_still_live_reference() {
    // `select (result (ref $s)) $r $q $cond` consumes each arm by value; the
    // chosen arm is a local read that is *also* read again after the select, so
    // the arm must clone rather than move. `$r` holds 10, `$q` holds 20: with
    // cond=1 the result is `r + r = 20`, with cond=0 it is `q + r = 30`.
    compile_run(
        "gc_reuse_select",
        r#"(module
            (type $s (struct (field i32)))
            (func (export "f") (param $cond i32) (result i32)
              (local $r (ref $s)) (local $q (ref $s)) (local $picked (ref $s))
              (local.set $r (struct.new $s (i32.const 10)))
              (local.set $q (struct.new $s (i32.const 20)))
              (local.set $picked
                (select (result (ref $s))
                  (local.get $r) (local.get $q) (local.get $cond)))
              (i32.add
                (struct.get $s 0 (local.get $picked))
                (struct.get $s 0 (local.get $r)))))"#,
        "assert_eq!(func0(1), 20); assert_eq!(func0(0), 30);",
    );
}

// NOTE: reference-typed *payloads* — exception tags (`throw`/`catch`),
// continuation tag results (`suspend`/`resume`), and `cont.bind` — are rejected
// at transpile time (`Unsupported("... reference type")`) because a handle
// cannot be erased into the i64 payload slots. They therefore cannot exhibit
// this move/clone hazard and are intentionally not probed here.

#[test]
fn multi_value_return_yields_two_copies_of_one_reference() {
    // A function returning `(ref $s) (ref $s)` from the same local pushes the
    // handle twice; both copies must be cloned, not moved. The caller reads a
    // field through each returned reference.
    compile_run(
        "gc_reuse_multi_return",
        r#"(module
            (type $s (struct (field i32)))
            (func $dup (param (ref $s)) (result (ref $s) (ref $s))
              (local.get 0) (local.get 0))
            (func (export "f") (param i32) (result i32)
              (local $r (ref $s)) (local $x (ref $s)) (local $y (ref $s))
              (local.set $r (struct.new $s (local.get 0)))
              (call $dup (local.get $r))
              (local.set $y)
              (local.set $x)
              (i32.add
                (struct.get $s 0 (local.get $x))
                (struct.get $s 0 (local.get $y)))))"#,
        "assert_eq!(func1(21), 42);",
    );
}
