//! End-to-end tests for whole-module *scenarios*: feature combinations and
//! instantiation behaviour that single-feature suites do not cover.

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

use common::{compile_run, compile_run_raw};

#[test]
fn start_function_is_currently_not_run() {
    // KNOWN LIMITATION: the `start` section is not implemented — a module's
    // start function is silently ignored, so state it would set up is never
    // applied. This test pins that behaviour: `$init` would set the global to
    // 99, but after instantiation the getter still returns the initial 7.
    //
    // When `start` support lands, this test SHOULD fail; update it to expect 99.
    compile_run(
        "scen_start",
        r#"
        (module
          (global $g (mut i32) (i32.const 7))
          (func $init (global.set $g (i32.const 99)))
          (func (export "get") (result i32) (global.get $g))
          (start $init))
        "#,
        // func0 = $init, func1 = get.
        "let mut inst = Instance::new();\n    \
         assert_eq!(inst.func1(), 7, \"start is not yet run; see KNOWN LIMITATION\");",
    );
}

#[test]
fn imported_memory_and_defined_table_coexist() {
    // A module that imports its linear memory from the host *and* defines its
    // own funcref table with `call_indirect`. The dispatched result is written
    // through the host memory and read back, exercising both channels together.
    compile_run_raw(
        "scen_mem_table",
        r#"
        (module
          (import "env" "mem" (memory 1))
          (type $sig (func (param i32) (result i32)))
          (table 2 funcref)
          (elem (i32.const 0) $inc $dbl)
          (func $inc (param i32) (result i32) (i32.add (local.get 0) (i32.const 1)))
          (func $dbl (param i32) (result i32) (i32.mul (local.get 0) (i32.const 2)))
          (func (export "run") (param $slot i32) (param $x i32) (result i32)
            (i32.store (i32.const 0)
              (call_indirect (type $sig) (local.get $x) (local.get $slot)))
            (i32.load (i32.const 0))))
        "#,
        r#"
        struct Host { mem: Vec<u8> }
        impl Imports for Host {
            fn memory(&self) -> &[u8] { &self.mem }
            fn memory_mut(&mut self) -> &mut Vec<u8> { &mut self.mem }
        }
        fn main() {
            // func2 = run; slot 0 = $inc, slot 1 = $dbl.
            let mut inst = Instance::new(Host { mem: vec![0u8; 65536] });
            assert_eq!(inst.func2(0, 10), 11);
            assert_eq!(inst.func2(1, 10), 20);
        }
        "#,
    );
}

#[test]
fn exception_unwinds_across_a_tail_call() {
    // Feature interaction: a `throw` must unwind correctly across a
    // `return_call` boundary. `$mid` tail-calls `$thrower`, which throws; the
    // exception has to propagate up through the tail-called frame to the
    // caller's `catch $e`, yielding the thrown payload.
    compile_run(
        "scen_eh_tail_call",
        r#"
        (module
          (tag $e (param i32))
          (func $thrower (param i32) (result i32)
            (local.get 0) (throw $e))
          (func $mid (param i32) (result i32)
            (return_call $thrower (local.get 0)))
          (func (export "f") (param i32) (result i32)
            try (result i32)
              local.get 0
              call $mid
            catch $e
            end))
        "#,
        // $thrower=func0, $mid=func1, f=func2; stateless -> free functions.
        "assert_eq!(func2(5), 5); assert_eq!(func2(-7), -7);",
    );
}

#[test]
fn exception_preserves_memory_side_effects_before_the_throw() {
    // Feature interaction: side effects committed inside a `try` before a
    // `throw` must survive the unwind. The store to `mem[0]` happens, then the
    // throw is caught; reading the cell back afterwards observes the written 42.
    compile_run(
        "scen_eh_memory",
        r#"
        (module
          (memory 1)
          (tag $e)
          (func (export "f") (result i32)
            try
              i32.const 0 i32.const 42 i32.store
              throw $e
            catch_all end
            i32.const 0 i32.load))
        "#,
        // Stateful (memory) -> Instance method; f is func0.
        "let mut inst = Instance::new(); assert_eq!(inst.func0(), 42);",
    );
}

#[test]
fn tail_call_from_inside_an_if_within_a_block() {
    // Feature interaction: `return_call` in tail position of a nested `if`
    // inside a `block`. The tail-call diverges from the enclosing structured
    // control flow (never yielding a block result), while the `else` arm falls
    // through normally.
    compile_run(
        "scen_tail_call_nested",
        r#"
        (module
          (func $base (param i32) (result i32) (local.get 0))
          (func (export "f") (param i32) (result i32)
            (block $b (result i32)
              (if (result i32) (i32.gt_s (local.get 0) (i32.const 0))
                (then (return_call $base (local.get 0)))
                (else (i32.const -1))))))
        "#,
        // $base=func0, f=func1; stateless -> free functions.
        "assert_eq!(func1(5), 5); assert_eq!(func1(-3), -1);",
    );
}

#[test]
fn tail_call_from_inside_a_loop() {
    // Feature interaction: `return_call` reached from within a `loop`. The loop
    // counts down and, on hitting zero, tail-calls `$base` from inside the loop
    // body — the tail-call must exit the whole function, not merely the loop.
    compile_run(
        "scen_tail_call_loop",
        r#"
        (module
          (func $base (param i32) (result i32)
            (i32.mul (local.get 0) (i32.const 10)))
          (func (export "f") (param i32) (result i32)
            (loop $l (result i32)
              (if (result i32) (i32.eqz (local.get 0))
                (then (return_call $base (local.get 0)))
                (else
                  (local.set 0 (i32.sub (local.get 0) (i32.const 1)))
                  (br $l))))))
        "#,
        // $base=func0, f=func1; stateless -> free functions.
        "assert_eq!(func1(0), 0); assert_eq!(func1(3), 0);",
    );
}

#[test]
fn gc_reference_mutated_through_a_callee_is_shared() {
    // Feature interaction: a heap object handle passed to a callee names the
    // *same* object, so a `struct.set` in the callee is observed by the caller.
    // `$bump` increments the mutable field; calling it twice on the same handle
    // leaves the field at n + 2 when the caller reads it back.
    compile_run(
        "scen_gc_shared_ref",
        r#"
        (module
          (type $s (struct (field (mut i32))))
          (func $bump (param (ref $s))
            (local.get 0)
            (i32.add (struct.get $s 0 (local.get 0)) (i32.const 1))
            (struct.set $s 0))
          (func (export "f") (param i32) (result i32)
            (local $r (ref $s))
            (local.set $r (struct.new $s (local.get 0)))
            (call $bump (local.get $r))
            (call $bump (local.get $r))
            (struct.get $s 0 (local.get $r))))
        "#,
        // $bump=func0, f=func1; GC alone does not make the module stateful.
        "assert_eq!(func1(5), 7); assert_eq!(func1(0), 2);",
    );
}

#[test]
fn continuation_throw_propagates_to_the_resumer() {
    // Feature interaction: an exception thrown *inside* a continuation body
    // unwinds out through `resume` to the resumer's `catch`. `$gen` throws `$e`
    // with payload 99 on its first step; the driver resumes it inside a `try`,
    // and the exception surfaces at `catch $e` carrying 99.
    compile_run(
        "scen_cont_throw",
        r#"
        (module
          (type $ft (func (result i32)))
          (type $ct (cont $ft))
          (tag $yield (param i32))
          (tag $e (param i32))
          (func $gen (result i32)
            i32.const 99 throw $e)
          (func (export "run") (result i32)
            (local $k (ref null $ct))
            ref.func $gen cont.new $ct local.set $k
            try (result i32)
              (block $on_yield (result i32 (ref $ct))
                local.get $k resume $ct (on $yield $on_yield)
                return)
              local.set $k
              drop
              i32.const -1
            catch $e
            end))
        "#,
        "let mut inst = Instance::new(); assert_eq!(inst.func1(), 99);",
    );
}

#[test]
fn gc_reference_local_survives_a_suspend() {
    // Feature interaction: a `(ref $s)` local created before a `suspend` must be
    // saved into the continuation frame and restored on resume, just like a
    // scalar local. `$gen` builds a struct holding 5, suspends with 10, then on
    // resume reads the surviving struct field back. The driver sums the yielded
    // 10 and the returned 5 -> 15.
    compile_run(
        "scen_cont_gc_local",
        r#"
        (module
          (type $s (struct (field (mut i32))))
          (type $ft (func (result i32)))
          (type $ct (cont $ft))
          (tag $yield (param i32))
          (func $gen (result i32)
            (local $r (ref $s))
            i32.const 5 struct.new $s local.set $r
            i32.const 10 suspend $yield
            local.get $r struct.get $s 0)
          (func (export "run") (result i32)
            (local $acc i32) (local $k (ref null $ct))
            ref.func $gen cont.new $ct local.set $k
            (loop $again
              (block $on_yield (result i32 (ref $ct))
                local.get $k resume $ct (on $yield $on_yield)
                local.get $acc i32.add return)
              local.set $k
              local.get $acc i32.add local.set $acc
              br $again)
            unreachable))
        "#,
        "let mut inst = Instance::new(); assert_eq!(inst.func1(), 15);",
    );
}

#[test]
fn two_passive_data_segments_initialise_independently() {
    // Two passive data segments feed the same memory. Initialising from each
    // lands its own bytes; dropping segment 0 makes a later `memory.init` from
    // it trap, while segment 1 remains usable.
    compile_run(
        "scen_passive2",
        r#"
        (module
          (memory 1)
          (data $a "AB")
          (data $b "CD")
          (func (export "init_a") (param $dst i32)
            (memory.init $a (local.get $dst) (i32.const 0) (i32.const 2)))
          (func (export "init_b") (param $dst i32)
            (memory.init $b (local.get $dst) (i32.const 0) (i32.const 2)))
          (func (export "drop_a") (data.drop $a))
          (func (export "load") (param $addr i32) (result i32)
            (i32.load8_u (local.get $addr))))
        "#,
        // func0 init_a, func1 init_b, func2 drop_a, func3 load.
        "let mut inst = Instance::new();\n    \
         inst.func0(0);\n    \
         inst.func1(2);\n    \
         assert_eq!(inst.func3(0), b'A' as i32);\n    \
         assert_eq!(inst.func3(1), b'B' as i32);\n    \
         assert_eq!(inst.func3(2), b'C' as i32);\n    \
         assert_eq!(inst.func3(3), b'D' as i32);\n    \
         inst.func2();\n    \
         inst.func1(4);\n    \
         assert_eq!(inst.func3(4), b'C' as i32);",
    );
}
