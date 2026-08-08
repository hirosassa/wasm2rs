//! Tests for the recommended `Cargo.toml` that `wasm2rs` emits alongside a
//! split crate.
//!
//! A split module is written as a `lib.rs` root plus `funcs_{n}.rs` chunks that
//! form one crate; on its own that crate has no build settings, so a consumer
//! would compile it at Cargo's defaults (`opt-level = 0`, unwinding, unstripped)
//! and get a needlessly large, slow binary. `cargo_manifest` supplies a
//! size-optimized-but-still-fast `[profile.release]` (opt-level 3 + thin LTO +
//! one codegen unit + abort-on-panic + strip) so the generated crate builds to a
//! compact release binary out of the box. These tests pin the profile keys and
//! the crate wiring (`[lib] path = "lib.rs"`) so a regression that dropped a
//! setting — silently inflating every consumer's binary — fails here.

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

use wasm2rs::cargo_manifest;

#[test]
fn manifest_names_the_package_and_points_lib_at_lib_rs() {
    let m = cargo_manifest("demo_module");
    assert!(
        m.contains("name = \"demo_module\""),
        "manifest should carry the given package name:\n{m}"
    );
    // The chunk root is `lib.rs` in the crate directory, not `src/lib.rs`, so the
    // manifest must redirect Cargo's library path away from its default.
    assert!(
        m.contains("[lib]") && m.contains("path = \"lib.rs\""),
        "manifest must point the lib target at lib.rs:\n{m}"
    );
    // Generated code depends only on `std`; the manifest carries no dependencies.
    assert!(
        !m.contains("[dependencies]"),
        "generated crate should have no dependencies:\n{m}"
    );
}

#[test]
fn manifest_release_profile_is_size_optimized_but_fast() {
    let m = cargo_manifest("m");
    // The balanced profile: opt-level 3 keeps the flattened dispatch fast, while
    // thin LTO, a single codegen unit, abort-on-panic (drops the unwinding
    // tables) and strip shrink the binary without trading away speed.
    for needle in [
        "[profile.release]",
        "opt-level = 3",
        "lto = \"thin\"",
        "codegen-units = 1",
        "panic = \"abort\"",
        "strip = true",
    ] {
        assert!(
            m.contains(needle),
            "release profile missing {needle:?}:\n{m}"
        );
    }
}

#[test]
fn manifest_is_stable_and_uses_edition_2024() {
    // The generated Rust is compiled at edition 2024 elsewhere in the test suite;
    // the manifest must agree so the emitted crate builds identically.
    let m = cargo_manifest("m");
    assert!(
        m.contains("edition = \"2024\""),
        "expected edition 2024:\n{m}"
    );
    // Deterministic output: the same package name yields byte-identical bytes.
    assert_eq!(cargo_manifest("m"), cargo_manifest("m"));
}
