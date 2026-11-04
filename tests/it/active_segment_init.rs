//! An active (or declared) segment is dropped right after instantiation, so a
//! later `memory.init`/`table.init` copies from an empty segment and
//! `data.drop`/`elem.drop` do nothing. An `i32` segment offset is unsigned: a
//! "negative" one is a huge address and traps at instantiation whenever it lies
//! past the end of the memory or table (any realistic one).

use crate::common;

const BODY: &str = "let mut inst = Instance::new();\n inst.func0();";
/// For modules whose exported `f` follows a helper function `$g`.
const BODY_AFTER_G: &str = "let mut inst = Instance::new();\n inst.func1();";

#[test]
fn memory_init_of_an_active_data_segment_copies_nothing() {
    common::compile_run(
        "asi_mem_init_zero",
        r#"(module (memory 1) (data (i32.const 0) "ab")
             (func (export "f") (memory.init 0 (i32.const 8) (i32.const 0) (i32.const 0))))"#,
        BODY,
    );
}

#[test]
fn memory_init_of_an_active_data_segment_traps_for_a_nonempty_range() {
    common::expect_trap(
        "asi_mem_init_trap",
        r#"(module (memory 1) (data (i32.const 0) "ab")
             (func (export "f") (memory.init 0 (i32.const 8) (i32.const 0) (i32.const 1))))"#,
        BODY,
    );
}

#[test]
fn data_drop_of_an_active_segment_is_a_no_op() {
    common::compile_run(
        "asi_data_drop",
        r#"(module (memory 1) (data (i32.const 0) "ab")
             (func (export "f") (data.drop 0) (data.drop 0)))"#,
        BODY,
    );
}

#[test]
fn table_init_of_an_active_elem_segment_copies_nothing() {
    common::compile_run(
        "asi_table_init_zero",
        r#"(module (table 4 funcref) (elem (i32.const 0) $g) (func $g)
             (func (export "f") (table.init 0 (i32.const 1) (i32.const 0) (i32.const 0))))"#,
        BODY_AFTER_G,
    );
}

#[test]
fn table_init_of_an_active_elem_segment_traps_for_a_nonempty_range() {
    common::expect_trap(
        "asi_table_init_trap",
        r#"(module (table 4 funcref) (elem (i32.const 0) $g) (func $g)
             (func (export "f") (table.init 0 (i32.const 1) (i32.const 0) (i32.const 1))))"#,
        BODY_AFTER_G,
    );
}

#[test]
fn elem_drop_of_active_and_declared_segments_is_a_no_op() {
    common::compile_run(
        "asi_elem_drop",
        r#"(module (table 4 funcref) (elem (i32.const 0) $g) (elem declare func $g) (func $g)
             (func (export "f") (elem.drop 0) (elem.drop 1)))"#,
        BODY_AFTER_G,
    );
}

#[test]
fn a_negative_data_offset_traps_at_instantiation() {
    common::expect_trap(
        "asi_neg_data",
        r#"(module (memory 1) (data (i32.const -1) "x") (func (export "f")))"#,
        BODY,
    );
}

#[test]
fn a_negative_i64_data_offset_traps_at_instantiation() {
    common::expect_trap(
        "asi_neg_i64_data",
        r#"(module (memory i64 1) (data (i64.const -1) "x") (func (export "f")))"#,
        BODY,
    );
}

#[test]
fn an_empty_i64_data_segment_past_the_end_traps_at_instantiation() {
    common::expect_trap(
        "asi_empty_i64_past_end",
        r#"(module (memory i64 1) (data (i64.const -65536) "") (func (export "f")))"#,
        BODY,
    );
}

#[test]
fn an_in_bounds_i64_data_offset_does_not_trap() {
    common::compile_run(
        "asi_i64_in_bounds",
        r#"(module (memory i64 1) (data (i64.const 65535) "x") (func (export "f")))"#,
        BODY,
    );
}

#[test]
fn a_negative_elem_offset_traps_at_instantiation() {
    common::expect_trap(
        "asi_neg_elem",
        r#"(module (table 4 funcref) (elem (i32.const -1) $g) (func $g) (func (export "f")))"#,
        BODY,
    );
}

#[test]
fn an_empty_data_segment_past_the_end_traps_at_instantiation() {
    common::expect_trap(
        "asi_empty_past_end",
        r#"(module (memory 1) (data (i32.const 65537) "") (func (export "f")))"#,
        BODY,
    );
}

#[test]
fn an_empty_elem_segment_past_the_end_traps_at_instantiation() {
    common::expect_trap(
        "asi_empty_elem_past_end",
        r#"(module (table 4 funcref) (elem (i32.const 5) func) (func (export "f")))"#,
        BODY,
    );
}

#[test]
fn an_empty_elem_segment_at_the_end_does_not_trap() {
    common::compile_run(
        "asi_empty_elem_at_end",
        r#"(module (table 4 funcref) (elem (i32.const 4) func) (func (export "f")))"#,
        BODY,
    );
}

fn transpile_err(wat: &str) -> String {
    let wasm = wat::parse_str(wat).expect("valid wat");
    wasm2rs::transpile(&wasm)
        .expect_err("must be rejected")
        .to_string()
}

#[test]
fn a_segment_index_out_of_range_is_rejected_not_read_as_empty() {
    for body in [
        "(memory.init 5 (i32.const 0) (i32.const 0) (i32.const 0))",
        "(data.drop 5)",
        "(table.init 5 (i32.const 0) (i32.const 0) (i32.const 0))",
        "(elem.drop 5)",
    ] {
        let err = transpile_err(&format!(
            "(module (memory 1) (table 1 funcref) (data (i32.const 0) \"a\") (func {body}))"
        ));
        assert!(err.contains("segment 5"), "{body}: {err}");
    }
}
