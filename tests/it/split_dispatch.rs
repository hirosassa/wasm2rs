//! A flattened `loop { match pc { … } }` dispatch whose arm count exceeds
//! [`TranspileOptions::split_dispatch`] is emitted as several sibling part
//! functions over a shared state struct instead of one giant function, so a
//! pathologically large flattened function becomes many smaller ones the Rust
//! backend can optimise (and codegen-parallelise) independently — without
//! changing what it computes. These tests pin that the split (a) happens (the
//! output carries the part functions and the state struct) and (b) still runs
//! identically once compiled.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::unwrap_in_result,
    reason = "test code"
)]

use std::process::Command;

use crate::common;

/// The block nest of a `br_table` switch over `cases` targets, nested
/// `cases + 2` blocks deep so the enclosing function is past the flatten
/// threshold (40). Each case sets local `$r` to its index and branches to the
/// shared exit; an out-of-range selector falls through to the default (`$r =
/// 999`). The `br_table`'s many successors keep every case a distinct,
/// non-fusible dispatch arm, so the flattened function has ~`cases` arms — a
/// controlled way to exceed a small `split_dispatch` threshold. The caller wraps
/// this with the module/function that reads `$r`.
fn switch_body(cases: usize) -> String {
    // An outer `$exit` lets each in-range case skip the default body: cases branch
    // to `$exit`, only the `$default` target falls through to the `r = 999` line.
    let mut body = String::from("(block $exit\n(block $default\n");
    for i in (0..cases).rev() {
        body.push_str(&format!("(block $c{i}\n"));
    }
    let labels: String = (0..cases).map(|i| format!("$c{i} ")).collect();
    body.push_str(&format!("(br_table {labels}$default (local.get $sel))\n"));
    for i in 0..cases {
        body.push_str(")\n");
        body.push_str(&format!("(local.set $r (i32.const {i})) (br $exit)\n"));
    }
    body.push_str(")\n");
    body.push_str("(local.set $r (i32.const 999))\n");
    body.push_str(")\n");
    body
}

/// A stateless module: `func0(sel)` returns the switch result directly.
fn stateless_switch(cases: usize) -> String {
    format!(
        "(module (func (export \"f\") (param $sel i32) (result i32) (local $r i32)\n\
         {}(local.get $r)))",
        switch_body(cases)
    )
}

/// A stateful module (a mutable global makes it a `struct Instance` with
/// methods): `func0(sel)` stashes the switch result in the global and returns it,
/// so the flattened dispatch is a `&mut self` method — the path whose split
/// bubbles the state struct to the crate root.
fn stateful_switch(cases: usize) -> String {
    format!(
        "(module (global $g (mut i32) (i32.const 0))\n\
         (func (export \"f\") (param $sel i32) (result i32) (local $r i32)\n\
         {}(global.set $g (local.get $r)) (global.get $g)))",
        switch_body(cases)
    )
}

/// Transpile `wat` with a `split_dispatch` cap of `max_arms_per_part`.
fn transpile_split(wat: &str, max_arms_per_part: usize) -> String {
    let wasm = wat::parse_str(wat).expect("valid wat");
    let opts = wasm2rs::TranspileOptions {
        split_dispatch: max_arms_per_part,
        ..Default::default()
    };
    wasm2rs::transpile_with_options(&wasm, &opts).expect("transpile ok")
}

/// Compile `program` (a standalone Rust source with its own `fn main`) warning-
/// free and run it, asserting it exits 0 — the same gate `common::build` uses.
fn build_and_run(tag: &str, program: &str) {
    let bin = common::build(tag, program);
    let run = Command::new(&bin).status().expect("run generated binary");
    assert!(run.success(), "generated program assertions failed: {tag}");
}

#[test]
fn stateless_dispatch_splits_into_parts_and_runs_identically() {
    let cases = 45; // > FLATTEN_DEPTH_THRESHOLD (40): the function flattens.
    let generated = transpile_split(&stateless_switch(cases), 8);

    // It flattened, and the flattened dispatch was split into sibling free
    // functions that dispatch on the shared state's `pc` field.
    assert!(
        generated.contains("match st.pc {"),
        "expected a flat dispatch over shared state:\n{generated}"
    );
    assert!(
        generated.contains("fn func0_part0(st: &mut S0)"),
        "expected the dispatch split into part functions:\n{generated}"
    );
    assert!(
        generated.contains("struct S0 {"),
        "expected a synthesised state struct:\n{generated}"
    );

    // The split must compute exactly what the un-split function does: case k sets
    // the result to k, an out-of-range selector to 999.
    let program = format!(
        "{generated}\n\
         fn main() {{\n\
         \x20   assert_eq!(func0(0), 0);\n\
         \x20   assert_eq!(func0(7), 7);\n\
         \x20   assert_eq!(func0(44), 44);\n\
         \x20   assert_eq!(func0(45), 999);\n\
         \x20   assert_eq!(func0(1000), 999);\n\
         }}\n"
    );
    build_and_run("split_dispatch_stateless", &program);
}

#[test]
fn stateful_method_dispatch_splits_with_root_struct_and_runs_identically() {
    let cases = 45;
    let generated = transpile_split(&stateful_switch(cases), 8);

    // The method's parts take `&mut self` alongside the state, and the shared
    // struct is emitted at the crate root (not inside the `impl`).
    assert!(
        generated.contains("fn func0_part0(&mut self, st: &mut S0)"),
        "expected `&mut self` part functions:\n{generated}"
    );
    assert!(
        generated.contains("struct S0 {"),
        "expected a synthesised state struct:\n{generated}"
    );
    // The struct sits at module scope, ahead of the `impl` that references it.
    let struct_at = generated.find("struct S0 {").expect("struct present");
    let impl_at = generated.find("impl").expect("impl present");
    assert!(
        struct_at < impl_at,
        "state struct must precede the impl block:\n{generated}"
    );

    let program = format!(
        "{generated}\n\
         fn main() {{\n\
         \x20   let mut inst = Instance::new();\n\
         \x20   assert_eq!(inst.func0(0), 0);\n\
         \x20   assert_eq!(inst.func0(7), 7);\n\
         \x20   assert_eq!(inst.func0(44), 44);\n\
         \x20   assert_eq!(inst.func0(45), 999);\n\
         \x20   assert_eq!(inst.func0(1000), 999);\n\
         }}\n"
    );
    build_and_run("split_dispatch_stateful", &program);
}

#[test]
fn dispatch_state_is_array_banked_not_one_field_per_temp() {
    // The real guest's giant flattened function hoists ~19.5k temps. Emitting one
    // struct field per temp made `#[derive(Default)]` expand to a ~19.5k-element
    // literal, and rustc's typeck/borrowck over a ~19.5k-field struct went super-
    // linear (a single-crate build ballooned from ~6min to >36min). Banking the
    // Copy-typed temps into a handful of typed arrays keeps the struct a few fields
    // wide and the `Default` a few array-repeats, regardless of temp count.
    let cases = 45;
    let generated = transpile_split(&stateless_switch(cases), 8);

    // `#[derive(Default)]` is gone; a hand-written impl initialises each bank with
    // a single array-repeat rather than one `Default::default()` call per temp.
    assert!(
        generated.contains("impl Default for S0 {"),
        "expected a hand-written Default over array banks:\n{generated}"
    );
    // The i32 temps (e.g. the `$r` local) share one array bank, not a field each.
    assert!(
        generated.contains("bank_i32: [i32;"),
        "expected an i32 array bank field:\n{generated}"
    );
    assert!(
        generated.contains("st.bank_i32["),
        "expected temp reads/writes to index the bank:\n{generated}"
    );

    // Banking must not change what the dispatch computes.
    let program = format!(
        "{generated}\n\
         fn main() {{\n\
         \x20   assert_eq!(func0(0), 0);\n\
         \x20   assert_eq!(func0(44), 44);\n\
         \x20   assert_eq!(func0(45), 999);\n\
         }}\n"
    );
    build_and_run("split_dispatch_banked", &program);
}
