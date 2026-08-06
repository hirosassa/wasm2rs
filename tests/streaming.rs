//! Tests for the streaming / bounded-output path (`transpile_split`).
//!
//! Phase 3's goal is that a huge module no longer has to be held in memory as
//! one string: each file is emitted to the sink and dropped, and every chunk
//! holds at most `funcs_per_file` functions. We cannot assert peak RSS directly,
//! but we can pin the observable consequences: the number and ordering of
//! emitted files, and that no chunk ever exceeds the requested cap (so the work
//! held at any instant is bounded, regardless of the module's total size).

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

mod common;

use common::{transpile_files, transpile_files_capped};

/// The number of `pub fn func` definitions in one emitted file — one per wasm
/// function, whether it is a free function (stateless) or an inherent method
/// (stateful).
fn func_count(code: &str) -> usize {
    code.matches("pub fn func").count()
}

/// A stateless module with five functions. `funcs_per_file = 1` must yield five
/// single-function chunk files followed by the `lib.rs` root, emitted in that
/// order (the root is written last, once the used-helper sets are complete).
#[test]
fn one_function_per_file_streams_each_chunk_then_the_root() {
    let wat = r#"(module
        (func (result i32) i32.const 0)
        (func (result i32) i32.const 1)
        (func (result i32) i32.const 2)
        (func (result i32) i32.const 3)
        (func (result i32) i32.const 4))"#;

    let files = transpile_files(wat, 1);
    let names: Vec<&str> = files.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "funcs_0.rs",
            "funcs_1.rs",
            "funcs_2.rs",
            "funcs_3.rs",
            "funcs_4.rs",
            "lib.rs"
        ],
    );

    // Every chunk holds exactly one function; the root holds none.
    for f in &files {
        let expected = if f.name == "lib.rs" { 0 } else { 1 };
        assert_eq!(
            func_count(&f.code),
            expected,
            "unexpected function count in {}",
            f.name,
        );
    }

    // The root wires up every chunk module.
    let root = &files.last().unwrap().code;
    for i in 0..5 {
        assert!(
            root.contains(&format!("mod funcs_{i};")),
            "root is missing `mod funcs_{i};`\n{root}",
        );
    }
}

/// With a cap of two functions and five functions total, the split is
/// `ceil(5 / 2) = 3` chunks of sizes 2, 2, 1 — no chunk ever exceeds the cap, so
/// the per-file work stays bounded no matter how many functions the module has.
#[test]
fn no_chunk_exceeds_the_requested_cap() {
    let wat = r#"(module
        (func (result i32) i32.const 0)
        (func (result i32) i32.const 1)
        (func (result i32) i32.const 2)
        (func (result i32) i32.const 3)
        (func (result i32) i32.const 4))"#;

    let files = transpile_files(wat, 2);

    let chunk_sizes: Vec<usize> = files
        .iter()
        .filter(|f| f.name != "lib.rs")
        .map(|f| func_count(&f.code))
        .collect();

    assert_eq!(chunk_sizes, vec![2, 2, 1]);
    assert!(
        chunk_sizes.iter().all(|&n| n <= 2),
        "a chunk exceeded the requested cap: {chunk_sizes:?}",
    );
}

/// The byte cap bounds chunk size independently of the function count: a few
/// functions whose combined source exceeds a tiny cap must be spread across
/// several files even when `funcs_per_file` would keep them together. This is
/// the cap that actually limits peak memory when a module has a handful of very
/// large functions (a fixed count can still sum to an enormous file).
#[test]
fn a_byte_cap_splits_by_size_not_just_count() {
    let wat = r#"(module
        (func (result i32) i32.const 0)
        (func (result i32) i32.const 1)
        (func (result i32) i32.const 2)
        (func (result i32) i32.const 3))"#;

    // A 1-byte cap forces a flush after every function even though the count
    // limit (1000) would never trigger: four functions become four chunks.
    let files = transpile_files_capped(wat, 1000, 1);
    let chunk_sizes: Vec<usize> = files
        .iter()
        .filter(|f| f.name != "lib.rs")
        .map(|f| func_count(&f.code))
        .collect();
    assert_eq!(chunk_sizes, vec![1, 1, 1, 1]);
    assert_eq!(files.last().unwrap().name, "lib.rs");
}

/// A module that fits within the cap is emitted as a single `lib.rs`, so callers
/// that do not need splitting pay no structural overhead. Its contents match the
/// single-string `transpile` byte-for-byte.
#[test]
fn a_module_within_the_cap_stays_a_single_file() {
    let wat = r#"(module
        (func (export "a") (result i32) i32.const 1)
        (func (export "b") (result i32) i32.const 2))"#;

    let files = transpile_files(wat, 10);
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].name, "lib.rs");

    let wasm = wat::parse_str(wat).expect("valid wat");
    let single = wasm2rs::transpile(&wasm).expect("transpile ok");
    assert_eq!(files[0].code, single);
}
