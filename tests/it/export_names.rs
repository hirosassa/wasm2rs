//! A function exported as `wasi_thread_start` gets a name-addressable entry
//! point (wasi-threads needs to find it by name); all other functions keep
//! their `func{N}` names and no other export gains a wrapper.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::arithmetic_side_effects,
    reason = "test code"
)]

use crate::common;

use common::{compile_run, compile_run_split, transpile_files};
use wasm2rs::{TranspileError, transpile};

#[test]
fn stateful_wasi_thread_start_is_callable_by_name() {
    // `wasi_thread_start(tid, arg)` stores `tid + arg` at address 0; the
    // by-name method must reach the very same body as `func{N}`.
    compile_run(
        "export_wasi_thread_start_stateful",
        r#"(module
            (memory 1)
            (func (export "wasi_thread_start") (param i32 i32)
              i32.const 0
              local.get 0
              local.get 1
              i32.add
              i32.store)
            (func (export "get") (result i32)
              i32.const 0 i32.load))"#,
        r#"let mut inst = Instance::new();
        inst.wasi_thread_start(40, 2);
        assert_eq!(inst.func1(), 42);"#,
    );
}

#[test]
fn stateless_wasi_thread_start_is_a_free_function() {
    compile_run(
        "export_wasi_thread_start_stateless",
        r#"(module
            (func (export "wasi_thread_start") (param i32 i32)
              local.get 0 local.get 1 i32.add drop))"#,
        "wasi_thread_start(1, 2);",
    );
}

#[test]
fn wasi_thread_start_is_emitted_in_split_crates() {
    // With one function per file the entry point must live in the root, for
    // both the stateless (free fn) and the stateful (method) layouts.
    let names = compile_run_split(
        "export_wasi_thread_start_split_stateless",
        r#"(module
            (func (export "wasi_thread_start") (param i32 i32)
              local.get 0 local.get 1 i32.add drop)
            (func (export "other") (result i32) i32.const 1))"#,
        1,
        "wasi_thread_start(1, 2);",
    );
    assert!(names.len() > 2, "expected a split crate, got {names:?}");
    compile_run_split(
        "export_wasi_thread_start_split_stateful",
        r#"(module
            (memory 1)
            (func (export "wasi_thread_start") (param i32 i32)
              i32.const 0 local.get 0 local.get 1 i32.add i32.store)
            (func (export "get") (result i32) i32.const 0 i32.load))"#,
        1,
        "let mut inst = Instance::new(); inst.wasi_thread_start(3, 4); assert_eq!(inst.func1(), 7);",
    );
}

#[test]
fn modules_without_wasi_thread_start_gain_no_wrapper() {
    let files = transpile_files(
        r#"(module
            (memory 1)
            (func (export "start") (param i32 i32))
            (func (export "get") (result i32) i32.const 0 i32.load))"#,
        0,
    );
    for f in &files {
        assert!(
            !f.code.contains("wasi_thread_start"),
            "unexpected wrapper in {}",
            f.name
        );
    }
}

#[test]
fn wasi_thread_start_with_the_wrong_signature_is_rejected() {
    let wasm = wat::parse_str(r#"(module (func (export "wasi_thread_start") (param i32)))"#)
        .expect("valid wat");
    match transpile(&wasm).as_ref().map_err(TranspileError::root) {
        Err(TranspileError::Unsupported(msg)) => assert!(
            msg.contains("wasi_thread_start"),
            "wrong rejection reason: {msg:?}"
        ),
        other => panic!("expected Unsupported, got {other:?}"),
    }
}
