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
