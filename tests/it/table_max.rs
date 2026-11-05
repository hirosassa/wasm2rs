//! `table.grow` must not grow a table past its declared maximum: it returns -1
//! and leaves the table unchanged. A table without a maximum still grows.

use crate::common;

/// Funcs: grow=0, size=1.
const MAX2: &str = r#"(module
  (table 1 2 funcref)
  (func (export "grow") (param i32) (result i32)
    ref.null func local.get 0 table.grow 0)
  (func (export "size") (result i32) table.size 0))"#;

#[test]
fn grow_stops_at_the_declared_maximum() {
    common::compile_run(
        "table_max_stops",
        MAX2,
        "let mut inst = Instance::new();\n \
         assert_eq!(inst.func0(1), 1);\n \
         assert_eq!(inst.func1(), 2);\n \
         assert_eq!(inst.func0(1), -1);\n \
         assert_eq!(inst.func1(), 2);",
    );
}

#[test]
fn growing_by_zero_at_the_maximum_succeeds() {
    common::compile_run(
        "table_max_zero",
        MAX2,
        "let mut inst = Instance::new();\n \
         assert_eq!(inst.func0(1), 1);\n \
         assert_eq!(inst.func0(0), 2);",
    );
}

#[test]
fn a_single_oversized_grow_fails_without_changing_the_table() {
    common::compile_run(
        "table_max_oversized",
        MAX2,
        "let mut inst = Instance::new();\n \
         assert_eq!(inst.func0(2), -1);\n \
         assert_eq!(inst.func1(), 1);",
    );
}

#[test]
fn an_externref_table_honors_its_maximum() {
    common::compile_run(
        "table_max_externref",
        r#"(module
          (table 0 1 externref)
          (func (export "grow") (param i32) (result i32)
            ref.null extern local.get 0 table.grow 0))"#,
        "let mut inst = Instance::new();\n \
         assert_eq!(inst.func0(2), -1);\n \
         assert_eq!(inst.func0(1), 0);\n \
         assert_eq!(inst.func0(1), -1);",
    );
}

#[test]
fn a_64_bit_table_honors_its_maximum() {
    common::compile_run(
        "table_max_i64",
        r#"(module
          (table i64 1 2 funcref)
          (func (export "grow") (param i64) (result i64)
            ref.null func local.get 0 table.grow 0))"#,
        "let mut inst = Instance::new();\n \
         assert_eq!(inst.func0(2), -1);\n \
         assert_eq!(inst.func0(1), 1);\n \
         assert_eq!(inst.func0(1), -1);",
    );
}

#[test]
fn a_table_without_a_maximum_still_grows() {
    common::compile_run(
        "table_max_none",
        r#"(module
          (table 1 funcref)
          (func (export "grow") (param i32) (result i32)
            ref.null func local.get 0 table.grow 0))"#,
        "let mut inst = Instance::new();\n \
         assert_eq!(inst.func0(100), 1);\n \
         assert_eq!(inst.func0(1), 101);",
    );
}
