//! End-to-end tests for GC phase G4: `extern.convert_any` / `any.convert_extern`
//! — the bridge between the `any` and `extern` reference hierarchies.
//!
//! An `externref` stays a `u32`; a non-null one internalised from an `anyref`
//! indexes a per-instance box (`extern_box: Vec<GcRef>`). `extern.convert_any`
//! boxes the managed handle and returns its index; `any.convert_extern` reads it
//! back. So an `any -> extern -> any` round trip preserves the reference
//! (including `Rc` identity), and null maps to the null `externref`
//! (`u32::MAX`) and back. Using either op forces the module stateful (the box
//! lives on the `Instance`). Internalising a host-provided `externref` is out of
//! scope.

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

use common::compile_run;

#[test]
fn any_extern_round_trip_preserves_a_struct() {
    // A struct internalised to `externref` and back to `anyref` is still the same
    // struct: casting to `(ref $s)` and reading its field recovers the value.
    compile_run(
        "gc_convert_round_trip",
        r#"(module
            (type $s (struct (field i32)))
            (func (export "rt") (result i32)
              (local $e externref)
              i32.const 42 struct.new $s
              extern.convert_any
              local.set $e
              local.get $e any.convert_extern
              ref.cast (ref $s)
              struct.get $s 0))"#,
        "let mut inst = Instance::new(); assert_eq!(inst.func0(), 42);",
    );
}

#[test]
fn null_maps_across_both_conversions() {
    // A null `anyref` externalises to the null `externref`, and a null
    // `externref` internalises to the null `anyref`; `ref.is_null` sees both.
    compile_run(
        "gc_convert_null",
        r#"(module
            (func (export "n1") (result i32)
              ref.null any extern.convert_any ref.is_null)
            (func (export "n2") (result i32)
              ref.null extern any.convert_extern ref.is_null))"#,
        "let mut inst = Instance::new(); \
         assert_eq!(inst.func0(), 1); assert_eq!(inst.func1(), 1);",
    );
}

#[test]
fn round_trip_preserves_reference_identity() {
    // The round-tripped handle is `ref.eq` to the original: `extern.convert_any`
    // boxes the same `Rc` and `any.convert_extern` clones it back.
    compile_run(
        "gc_convert_identity",
        r#"(module
            (type $s (struct (field i32)))
            (func (export "same") (result i32)
              (local $a (ref null any))
              i32.const 1 struct.new $s local.set $a
              local.get $a extern.convert_any any.convert_extern
              local.get $a
              ref.eq))"#,
        "let mut inst = Instance::new(); assert_eq!(inst.func0(), 1);",
    );
}
