//! End-to-end tests for the typed-continuations (stack-switching) proposal.
//!
//! Phase 1 only covers the type section and the null continuation reference: a
//! `(cont $ft)` type is accepted (it names an underlying function type), a
//! continuation reference lowers to a `u32` handle (`u32::MAX` is null, like a
//! `funcref`), and `ref.null`/`ref.is_null` work on it. Creating, resuming and
//! suspending continuations arrives in later phases.
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
fn cont_type_is_accepted_and_null_ref_is_null() {
    // Defining a `(cont $ft)` type must no longer be rejected, and `ref.null`
    // of that continuation type produces a null handle that `ref.is_null`
    // reports as null (1).
    compile_run(
        "cont_null_ref",
        r#"(module
            (type $ft (func (param i32) (result i32)))
            (type $ct (cont $ft))
            (func (export "f") (result i32)
              ref.null $ct ref.is_null))"#,
        "assert_eq!(func0(), 1);",
    );
}

#[test]
fn null_cont_local_defaults_to_null() {
    // A local typed `(ref null $ct)` defaults to the null handle, so reading it
    // back and testing `ref.is_null` reports null (1) without any assignment.
    compile_run(
        "cont_null_local",
        r#"(module
            (type $ft (func (result i32)))
            (type $ct (cont $ft))
            (func (export "f") (result i32)
              (local $k (ref null $ct))
              local.get $k ref.is_null))"#,
        "assert_eq!(func0(), 1);",
    );
}
