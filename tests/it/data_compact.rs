//! Active data segments are written into a fresh, zero-filled memory, so a
//! segment's leading/trailing zero bytes are not copied when that is provably
//! unobservable. These tests pin down every case where it is observable and the
//! zero-trimming must not apply: overlapping or reordered segments, out-of-bounds
//! tails that must still trap, passive segments, and host-owned memory.

use crate::common;

const BODY: &str = "let mut inst = Instance::new();\n inst.func0();";

fn generated(wat: &str) -> String {
    let wasm = wat::parse_str(wat).expect("valid wat");
    wasm2rs::transpile(&wasm).expect("transpile ok")
}

#[test]
fn zero_edges_of_a_segment_leave_the_memory_correct() {
    common::compile_run(
        "dc_zero_edges",
        r#"(module (memory 1) (data (i32.const 8) "\00\00ab\00\00")
             (func (export "f")
               (if (i32.ne (i32.load8_u (i32.const 7)) (i32.const 0)) (then unreachable))
               (if (i32.ne (i32.load8_u (i32.const 8)) (i32.const 0)) (then unreachable))
               (if (i32.ne (i32.load8_u (i32.const 10)) (i32.const 97)) (then unreachable))
               (if (i32.ne (i32.load8_u (i32.const 11)) (i32.const 98)) (then unreachable))
               (if (i32.ne (i32.load8_u (i32.const 12)) (i32.const 0)) (then unreachable))
               (if (i32.ne (i32.load8_u (i32.const 13)) (i32.const 0)) (then unreachable))))"#,
        BODY,
    );
}

#[test]
fn a_later_zero_run_still_overwrites_an_earlier_segment() {
    common::compile_run(
        "dc_overlap_zero",
        r#"(module (memory 1)
             (data (i32.const 0) "abcd")
             (data (i32.const 1) "\00\00")
             (func (export "f")
               (if (i32.ne (i32.load8_u (i32.const 0)) (i32.const 97)) (then unreachable))
               (if (i32.ne (i32.load8_u (i32.const 1)) (i32.const 0)) (then unreachable))
               (if (i32.ne (i32.load8_u (i32.const 2)) (i32.const 0)) (then unreachable))
               (if (i32.ne (i32.load8_u (i32.const 3)) (i32.const 100)) (then unreachable))))"#,
        BODY,
    );
}

#[test]
fn a_later_segment_wins_over_an_earlier_overlapping_one() {
    common::compile_run(
        "dc_overlap_order",
        r#"(module (memory 1)
             (data (i32.const 0) "\00ab")
             (data (i32.const 1) "XY\00")
             (func (export "f")
               (if (i32.ne (i32.load8_u (i32.const 1)) (i32.const 88)) (then unreachable))
               (if (i32.ne (i32.load8_u (i32.const 2)) (i32.const 89)) (then unreachable))
               (if (i32.ne (i32.load8_u (i32.const 3)) (i32.const 0)) (then unreachable))))"#,
        BODY,
    );
}

#[test]
fn a_zero_tail_past_the_end_still_traps() {
    common::expect_trap(
        "dc_zero_tail_oob",
        r#"(module (memory 1) (data (i32.const 65535) "a\00") (func (export "f")))"#,
        BODY,
    );
}

#[test]
fn an_all_zero_segment_past_the_end_still_traps() {
    common::expect_trap(
        "dc_all_zero_oob",
        r#"(module (memory 1) (data (i32.const 65535) "\00\00") (func (export "f")))"#,
        BODY,
    );
}

#[test]
fn a_zero_head_before_an_out_of_bounds_tail_still_traps() {
    common::expect_trap(
        "dc_zero_head_oob",
        r#"(module (memory 1) (data (i32.const 65530) "\00\00\00\00\00a\00\00") (func (export "f")))"#,
        BODY,
    );
}

#[test]
fn a_segment_ending_exactly_at_the_end_does_not_trap() {
    common::compile_run(
        "dc_exact_end",
        r#"(module (memory 1) (data (i32.const 65534) "\00a")
             (func (export "f")
               (if (i32.ne (i32.load8_u (i32.const 65535)) (i32.const 97)) (then unreachable))))"#,
        BODY,
    );
}

#[test]
fn memory_init_of_a_passive_segment_copies_its_zeros() {
    common::compile_run(
        "dc_passive_zeros",
        r#"(module (memory 1) (data "\00\00x\00")
             (func (export "f")
               (i32.store (i32.const 0) (i32.const -1))
               (memory.init 0 (i32.const 0) (i32.const 0) (i32.const 4))
               (if (i32.ne (i32.load8_u (i32.const 0)) (i32.const 0)) (then unreachable))
               (if (i32.ne (i32.load8_u (i32.const 1)) (i32.const 0)) (then unreachable))
               (if (i32.ne (i32.load8_u (i32.const 2)) (i32.const 120)) (then unreachable))
               (if (i32.ne (i32.load8_u (i32.const 3)) (i32.const 0)) (then unreachable))))"#,
        BODY,
    );
}

#[test]
fn a_second_memory_gets_its_own_trimmed_segment() {
    common::compile_run(
        "dc_two_memories",
        r#"(module (memory 1) (memory $m 1)
             (data (memory 0) (i32.const 4) "\00a\00")
             (data (memory $m) (i32.const 4) "\00b\00")
             (func (export "f")
               (if (i32.ne (i32.load8_u (i32.const 5)) (i32.const 97)) (then unreachable))
               (if (i32.ne (i32.load8_u $m (i32.const 5)) (i32.const 98)) (then unreachable))
               (if (i32.ne (i32.load8_u $m (i32.const 4)) (i32.const 0)) (then unreachable))))"#,
        BODY,
    );
}

#[test]
fn an_empty_in_bounds_segment_emits_no_copy() {
    let src = generated(r#"(module (memory 1) (data (i32.const 255) "") (func (export "f")))"#);
    assert!(!src.contains("copy_from_slice"), "{src}");
}

#[test]
fn an_all_zero_in_bounds_segment_emits_no_copy() {
    let src =
        generated(r#"(module (memory 1) (data (i32.const 8) "\00\00\00") (func (export "f")))"#);
    assert!(!src.contains("copy_from_slice"), "{src}");
}

#[test]
fn zero_bytes_are_spelled_with_a_short_escape() {
    let src =
        generated(r#"(module (memory 1) (data (i32.const 0) "a\00\00b") (func (export "f")))"#);
    assert!(!src.contains("\\x00"), "{src}");
}

#[test]
fn zero_edges_are_not_trimmed_in_a_host_owned_memory() {
    // The host's memory may hold non-zero bytes, so the zeros must be written.
    let src = generated(
        r#"(module (import "env" "memory" (memory 1)) (data (i32.const 8) "\00a\00")
             (func (export "f")))"#,
    );
    assert!(src.contains("[8..11].copy_from_slice"), "{src}");
}

#[test]
fn zero_edges_are_not_trimmed_in_a_shared_memory() {
    let src = generated(
        r#"(module (memory 1 1 shared) (data (i32.const 8) "\00a\00") (func (export "f")))"#,
    );
    assert!(src.contains("[8..11].copy_from_slice"), "{src}");
}

#[test]
fn a_runtime_offset_segment_is_not_trimmed() {
    let src = generated(
        r#"(module (import "env" "g" (global $g i32)) (memory 1)
             (data (global.get $g) "\00a\00") (func (export "f")))"#,
    );
    assert!(src.contains("saturating_add(3)"), "{src}");
}
