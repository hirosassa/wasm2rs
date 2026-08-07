//! Tests for rendering a branch-targeted `block`/`if` as a Rust labeled block.
//!
//! A wasm `block` (and `if`) never loops, so when it is a branch target it is
//! emitted as a labeled block `'lN: { … }` rather than `'lN: loop { … break; }`.
//! The labeled block falls through its end naturally, so the trailing
//! `break 'lN;` disappears. A `loop`, which has a back-edge, still needs
//! `'lN: loop { … }` and its fall-through `break`.

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

use common::compile_run;

fn transpile(wat: &str) -> String {
    let wasm = wat::parse_str(wat).expect("valid wat");
    wasm2rs::transpile(&wasm).expect("transpile ok")
}

#[test]
fn targeted_block_uses_labeled_block_without_trailing_break() {
    // `br_if 0` targets the enclosing block, making it a branch target. The block
    // falls through its end, so with a labeled block there is no trailing break.
    let wat = r#"(module (func (export "f") (param i32) (result i32)
        (block
            (br_if 0 (local.get 0))
            (return (i32.const 10)))
        (i32.const 20)))"#;
    let src = transpile(wat);

    assert!(
        src.contains("'l0: {") && !src.contains("'l0: loop {"),
        "a targeted block should render as a labeled block, not a loop\n{src}",
    );
    // The forward branch is still a `break` out of the labeled block.
    assert!(
        src.contains("break 'l0;"),
        "the br_if should still break out of the labeled block\n{src}",
    );
    // No trailing `break 'l0;` immediately before the block's closing brace: the
    // only break here is the guarded one inside the `if`.
    assert_eq!(
        src.matches("break 'l0;").count(),
        1,
        "a labeled block needs no trailing break, so only the guarded break remains\n{src}",
    );

    // br_if taken (arg != 0) falls through to 20; not taken returns 10.
    compile_run(
        "labeled_block_taken",
        wat,
        "assert_eq!(func0(1), 20);\n    assert_eq!(func0(0), 10);",
    );
}

#[test]
fn targeted_loop_still_uses_loop_with_trailing_break() {
    // A `loop` branched back to keeps `'lN: loop { … }`: `br 0` is a `continue`
    // back-edge, and the fall-through still needs a `break` to leave the loop.
    let wat = r#"(module (func (export "f") (param i32) (result i32)
        (local i32)
        (loop
            (local.set 1 (i32.add (local.get 1) (i32.const 1)))
            (br_if 0 (i32.lt_s (local.get 1) (local.get 0))))
        (local.get 1)))"#;
    let src = transpile(wat);

    assert!(
        src.contains("'l0: loop {"),
        "a targeted loop should still render as a labeled loop\n{src}",
    );
    assert!(
        src.contains("continue 'l0;"),
        "the back-edge br should be a continue\n{src}",
    );
    assert!(
        src.contains("break 'l0;"),
        "the loop still needs a trailing break to exit on fall-through\n{src}",
    );

    // Counts up to max(0, n): func0(3) -> 3, func0(0) -> 1 (one pass minimum).
    compile_run(
        "labeled_loop",
        wat,
        "assert_eq!(func0(3), 3);\n    assert_eq!(func0(0), 1);",
    );
}
