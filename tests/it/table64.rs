//! A 64-bit table (`table i64`) is indexed and sized by `i64`: `call_indirect`,
//! `table.get/set/fill`, the segment offsets and `table.copy`/`table.init`
//! take an `i64` index, and `table.size`/`table.grow` yield an `i64`. An
//! index above `u32::MAX` must trap rather than wrap into range.

use crate::common;

/// `call(i)` is `call_indirect` of slot `i`; slot 1 holds `$a`, which returns 7.
/// Funcs: `$a`=0, call=1, size=2, grow=3, is_null=4, set=5, fill=6, init=7.
const TABLE64: &str = r#"(module
  (type $ft (func (result i32)))
  (func $a (result i32) i32.const 7)
  (table $t i64 4 funcref)
  (elem (table $t) (i64.const 1) func $a)
  (elem $p func $a)
  (func (export "call") (param i64) (result i32)
    local.get 0 call_indirect $t (type $ft))
  (func (export "size") (result i64) table.size $t)
  (func (export "grow") (param i64) (result i64)
    ref.null func local.get 0 table.grow $t)
  (func (export "is_null") (param i64) (result i32)
    local.get 0 table.get $t ref.is_null)
  (func (export "set") (param i64)
    local.get 0 ref.func $a table.set $t)
  (func (export "fill") (param i64 i64)
    local.get 0 ref.func $a local.get 1 table.fill $t)
  (func (export "init") (param i64)
    local.get 0 (i32.const 0) (i32.const 1) table.init $t $p))"#;

#[test]
fn call_indirect_takes_an_i64_index() {
    common::compile_run(
        "t64_call",
        TABLE64,
        "let mut inst = Instance::new();\n assert_eq!(inst.func1(1), 7);",
    );
}

#[test]
fn a_null_slot_traps() {
    common::expect_trap(
        "t64_null_slot",
        TABLE64,
        "let mut inst = Instance::new(); inst.func1(0);",
    );
}

#[test]
fn an_index_above_u32_max_traps_instead_of_wrapping() {
    // 0x1_0000_0001 would wrap to slot 1 (which holds `$a`) if truncated to u32.
    common::expect_trap(
        "t64_wide_index",
        TABLE64,
        "let mut inst = Instance::new(); inst.func1(0x1_0000_0001);",
    );
}

#[test]
fn size_and_grow_are_i64() {
    common::compile_run(
        "t64_grow",
        TABLE64,
        "let mut inst = Instance::new();\n \
         assert_eq!(inst.func2(), 4i64);\n \
         assert_eq!(inst.func3(2), 4i64);\n \
         assert_eq!(inst.func2(), 6i64);\n \
         assert_eq!(inst.func3(-1), -1i64);\n \
         assert_eq!(inst.func2(), 6i64);",
    );
}

#[test]
fn get_set_and_fill_take_an_i64_index() {
    common::compile_run(
        "t64_get_set_fill",
        TABLE64,
        "let mut inst = Instance::new();\n \
         assert_eq!(inst.func4(3), 1);\n \
         inst.func5(3);\n \
         assert_eq!(inst.func4(3), 0);\n \
         assert_eq!(inst.func1(3), 7);\n \
         inst.func6(0, 4);\n \
         assert_eq!(inst.func1(0), 7);",
    );
}

#[test]
fn a_wide_set_index_traps() {
    common::expect_trap(
        "t64_wide_set",
        TABLE64,
        "let mut inst = Instance::new(); inst.func5(0x1_0000_0000);",
    );
}

#[test]
fn a_wide_fill_destination_traps() {
    common::expect_trap(
        "t64_wide_fill",
        TABLE64,
        "let mut inst = Instance::new(); inst.func6(0x1_0000_0000, 1);",
    );
}

#[test]
fn table_init_takes_an_i64_destination() {
    common::compile_run(
        "t64_init",
        TABLE64,
        "let mut inst = Instance::new();\n inst.func7(2);\n assert_eq!(inst.func1(2), 7);",
    );
}

#[test]
fn a_wide_init_destination_traps() {
    common::expect_trap(
        "t64_wide_init",
        TABLE64,
        "let mut inst = Instance::new(); inst.func7(0x1_0000_0000);",
    );
}

#[test]
fn a_negative_elem_offset_traps_at_instantiation() {
    common::expect_trap(
        "t64_neg_elem",
        r#"(module (table i64 4 funcref) (func $g)
             (elem (i64.const -1) $g) (func (export "f")))"#,
        "let mut inst = Instance::new(); inst.func1();",
    );
}

/// `$t32` (table 1) is a 32-bit table: copy from the 64-bit `$t64` into it, so
/// the destination index is `i32`, the source `i64` and the length `i32`.
const MIXED: &str = r#"(module
  (type $ft (func (result i32)))
  (func $a (result i32) i32.const 7)
  (table $t64 i64 4 funcref)
  (table $t32 4 funcref)
  (elem (table $t64) (i64.const 2) func $a)
  (func (export "copy_to32") (param i32 i64 i32)
    local.get 0 local.get 1 local.get 2 table.copy $t32 $t64)
  (func (export "copy_within64") (param i64 i64 i64)
    local.get 0 local.get 1 local.get 2 table.copy $t64 $t64)
  (func (export "call32") (param i32) (result i32)
    local.get 0 call_indirect $t32 (type $ft))
  (func (export "call64") (param i64) (result i32)
    local.get 0 call_indirect $t64 (type $ft)))"#;

#[test]
fn table_copy_between_a_64_bit_and_a_32_bit_table() {
    common::compile_run(
        "t64_copy_mixed",
        MIXED,
        "let mut inst = Instance::new();\n \
         inst.func1(0, 2, 1);\n \
         assert_eq!(inst.func3(0), 7);",
    );
}

#[test]
fn table_copy_within_a_64_bit_table() {
    common::compile_run(
        "t64_copy_within",
        MIXED,
        "let mut inst = Instance::new();\n \
         inst.func2(0, 2, 1);\n \
         assert_eq!(inst.func4(0), 7);",
    );
}

#[test]
fn a_wide_copy_range_traps() {
    common::expect_trap(
        "t64_wide_copy",
        MIXED,
        "let mut inst = Instance::new(); inst.func2(0x1_0000_0000, 2, 1);",
    );
}
