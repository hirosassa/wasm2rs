//! End-to-end tests for multi-file (split) output.
//!
//! Each test transpiles a module with a small `funcs_per_file`, writes the
//! resulting `lib.rs` + `funcs_{n}.rs` files as one crate, compiles it with
//! `rustc -D warnings`, and runs it. The oracle is behavioural: the split crate
//! must compute exactly what the single-file build would, so cross-chunk calls,
//! shared runtime helpers, instance methods and `call_indirect` dispatch all
//! have to resolve across module boundaries.

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

use common::compile_run_split;

/// A stateless module where each function calls the one before it. Splitting one
/// function per file forces `func2` (in its own chunk) to call `func1` (another
/// chunk), which calls `func0` (a third). If the root did not re-export the
/// chunk functions — or the chunks did not `use super::*` — this would not link.
#[test]
fn stateless_cross_chunk_calls_resolve() {
    let wat = r#"(module
        (func $inc (param i32) (result i32) local.get 0 i32.const 1 i32.add)
        (func $twice (param i32) (result i32) local.get 0 call $inc call $inc)
        (func $quad (param i32) (result i32) local.get 0 call $twice call $twice))"#;

    // Three defined functions, one per file, plus the lib.rs root.
    let names = compile_run_split("split_cross_chunk", wat, 1, "assert_eq!(func2(10), 14);");
    assert_eq!(
        names,
        vec!["funcs_0.rs", "funcs_1.rs", "funcs_2.rs", "lib.rs"],
        "expected one chunk per function followed by the root",
    );
}

/// A runtime helper (`f32.min`/`f32.max`, whose wasm semantics differ from
/// Rust's operators) is emitted once at the crate root. A function living in a
/// chunk must still reach it through the chunk's `use super::*`.
#[test]
fn stateless_runtime_helper_reachable_from_chunk() {
    let wat = r#"(module
        (func $lo (param f32 f32) (result f32) local.get 0 local.get 1 f32.min)
        (func $hi (param f32 f32) (result f32) local.get 0 local.get 1 f32.max))"#;

    compile_run_split(
        "split_rt_helper",
        wat,
        1,
        "assert_eq!(func0(1.0, 2.0), 1.0);\n    assert_eq!(func1(1.0, 2.0), 2.0);",
    );
}

/// A stateful module: memory load/store helpers and a mutable global. Splitting
/// one function per file puts each `&mut self` method in its own `impl Instance`
/// block; they must share the struct, the private memory helpers and the global
/// field defined at the root.
#[test]
fn stateful_instance_methods_span_chunks() {
    let wat = r#"(module
        (memory 1)
        (global $g (mut i32) (i32.const 0))
        (func $store (param i32 i32) local.get 0 local.get 1 i32.store)
        (func $load (param i32) (result i32) local.get 0 i32.load)
        (func $bump (result i32)
            global.get $g i32.const 1 i32.add global.set $g
            global.get $g))"#;

    compile_run_split(
        "split_stateful",
        wat,
        1,
        "let mut inst = Instance::new();\n    \
         inst.func0(0, 42);\n    \
         assert_eq!(inst.func1(0), 42);\n    \
         assert_eq!(inst.func2(), 1);\n    \
         assert_eq!(inst.func2(), 2);",
    );
}

/// `call_indirect` compiles to a `call_ref_t{ti}` dispatch method on the root
/// impl. When the calling function lives in a chunk and its possible targets
/// live in other chunks, the dispatch (root) and the targets (chunks) must all
/// see one another.
#[test]
fn call_indirect_dispatch_spans_chunks() {
    let wat = r#"(module
        (type $bin (func (param i32 i32) (result i32)))
        (table 2 funcref)
        (elem (i32.const 0) $add $sub)
        (func $add (type $bin) local.get 0 local.get 1 i32.add)
        (func $sub (type $bin) local.get 0 local.get 1 i32.sub)
        (func $dispatch (param i32 i32 i32) (result i32)
            local.get 0 local.get 1 local.get 2 call_indirect (type $bin)))"#;

    compile_run_split(
        "split_call_indirect",
        wat,
        1,
        "let mut inst = Instance::new();\n    \
         assert_eq!(inst.func2(3, 4, 0), 7);\n    \
         assert_eq!(inst.func2(10, 4, 1), 6);",
    );
}
