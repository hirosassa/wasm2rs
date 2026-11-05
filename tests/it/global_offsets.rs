//! A segment offset may read an earlier immutable defined global
//! (`(data (global.get $base) ..)`); it is evaluated at transpile time.

use crate::common;

#[test]
fn a_data_offset_may_read_a_defined_global() {
    common::compile_run(
        "go_data",
        r#"(module
             (memory 1)
             (global $base i32 (i32.const 8))
             (data (global.get $base) "\2a")
             (func (export "f") (param i32) (result i32) (i32.load8_u (local.get 0))))"#,
        "let mut inst = Instance::new();\n\
         assert_eq!(inst.func0(8), 42);\n\
         assert_eq!(inst.func0(7), 0);",
    );
}

#[test]
fn an_element_offset_may_read_a_defined_global() {
    common::compile_run(
        "go_elem",
        r#"(module
             (type $r (func (result i32)))
             (table 4 funcref)
             (global $base i32 (i32.const 2))
             (func $f (result i32) i32.const 5)
             (elem (global.get $base) $f)
             (func (export "call") (param i32) (result i32)
               (call_indirect (type $r) (local.get 0))))"#,
        "let mut inst = Instance::new();\n assert_eq!(inst.func1(2), 5);",
    );
}

#[test]
fn an_offset_may_combine_a_global_with_extended_const_arithmetic() {
    common::compile_run(
        "go_arith",
        r#"(module
             (memory 1)
             (global $base i32 (i32.const 8))
             (data (i32.add (global.get $base) (i32.const 1)) "\07")
             (func (export "f") (param i32) (result i32) (i32.load8_u (local.get 0))))"#,
        "let mut inst = Instance::new();\n assert_eq!(inst.func0(9), 7);",
    );
}

#[test]
fn a_memory64_data_offset_may_read_an_i64_global() {
    common::compile_run(
        "go_mem64",
        r#"(module
             (memory i64 1)
             (global $base i64 (i64.const 16))
             (data (global.get $base) "\09")
             (func (export "f") (param i64) (result i32) (i32.load8_u (local.get 0))))"#,
        "let mut inst = Instance::new();\n assert_eq!(inst.func0(16), 9);",
    );
}

#[test]
fn a_global_offset_past_the_end_traps_at_instantiation() {
    common::expect_trap(
        "go_trap",
        r#"(module
             (memory 1)
             (global $base i32 (i32.const 65536))
             (data (global.get $base) "x")
             (func (export "f")))"#,
        "let mut inst = Instance::new();\n inst.func0();",
    );
}

#[test]
fn a_global_offset_reading_a_mutable_global_is_rejected() {
    let wasm = wat::parse_str(
        r#"(module
             (memory 1)
             (global $base (mut i32) (i32.const 8))
             (data (global.get $base) "x"))"#,
    )
    .expect("valid wat");
    let err = wasm2rs::transpile(&wasm)
        .expect_err("must be rejected")
        .to_string();
    assert!(err.contains("data offset"), "{err}");
}
