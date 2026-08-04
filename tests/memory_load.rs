//! Behavioural tests for the multi-byte memory *read* helpers (`r16`, `r32`,
//! `r64`, `rf32`, `rf64`, …). These helpers read N little-endian bytes out of
//! linear memory; regardless of how the read is lowered (element-wise indexing
//! or a single range slice), it must return the same value and must still trap
//! on an out-of-bounds access. The value checks near a page boundary pin the
//! exact byte range each helper reads.

mod common;

use common::{compile_run, expect_trap_with};

#[test]
fn multi_byte_loads_read_the_right_bytes() {
    // Initialise the first eight bytes to 01..08, then read them back at each
    // width. Little-endian, so i64 = 0x0807060504030201, i32 = 0x04030201, etc.
    compile_run(
        "load_values",
        r#"(module
             (memory 1)
             (data (i32.const 0) "\01\02\03\04\05\06\07\08")
             (func (export "l64")  (result i64) (i64.load       (i32.const 0)))
             (func (export "l32")  (result i32) (i32.load       (i32.const 0)))
             (func (export "l16u") (result i32) (i32.load16_u   (i32.const 0)))
             (func (export "l16s") (result i32) (i32.load16_s   (i32.const 0)))
             (func (export "lf64") (result f64) (f64.load       (i32.const 0)))
             (func (export "lf32") (result f32) (f32.load       (i32.const 0))))"#,
        "let mut inst = Instance::new();\n    \
         assert_eq!(inst.func0(), 0x0807060504030201i64);\n    \
         assert_eq!(inst.func1(), 0x04030201i32);\n    \
         assert_eq!(inst.func2(), 0x0201i32);\n    \
         assert_eq!(inst.func3(), 0x0201i32);\n    \
         assert_eq!(inst.func4().to_bits(), 0x0807060504030201u64);\n    \
         assert_eq!(inst.func5().to_bits(), 0x04030201u32);",
    );
}

#[test]
fn load_at_the_last_valid_offset_succeeds() {
    // One page is 65536 bytes; an i64.load at 65528 reads bytes [65528, 65536)
    // — the last eight bytes, exactly in bounds.
    compile_run(
        "load_boundary_ok",
        r#"(module
             (memory 1)
             (func (export "f") (result i64) (i64.load (i32.const 65528))))"#,
        "let mut inst = Instance::new();\n    \
         assert_eq!(inst.func0(), 0);",
    );
}

#[test]
fn i64_load_past_the_page_end_traps() {
    // An i64.load at 65529 reads [65529, 65537) — one byte past the page.
    expect_trap_with(
        "load_i64_oob",
        r#"(module
             (memory 1)
             (func (export "f") (result i64) (i64.load (i32.const 65529))))"#,
        "let mut inst = Instance::new();\n    inst.func0();",
        "out of range for slice",
    );
}
