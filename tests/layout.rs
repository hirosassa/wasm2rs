//! Characterization tests pinning the *exact* byte layout of a multi-file
//! split.
//!
//! Phase 4a streams each function's source straight into its chunk file instead
//! of materialising it as an intermediate `String` and copying it again. The
//! generated text must stay byte-for-byte identical through that refactor, and
//! indentation in particular is invisible to `rustc` — the compile-and-run
//! tests in `split.rs` would happily accept a method body indented to the wrong
//! column. These tests pin the whole chunk file, so any drift in the chunk
//! prelude, the `impl Instance` wrapper, the per-function blank-line separator,
//! or the method-body indent is caught immediately.

mod common;

use common::transpile_files;

const ALLOW: &str =
    "#[allow(dead_code, unused_variables, unused_assignments, unused_mut, unused_parens)]";

/// The first chunk of a stateless two-function module, split one function per
/// file. A stateless chunk emits free `pub fn`s directly (each carrying its own
/// lint-suppression attribute) after the `use super::*;` glob.
#[test]
fn stateless_chunk_layout_is_byte_exact() {
    let wat = r#"(module
        (func (param i32) (result i32) local.get 0)
        (func (result i32) i32.const 7))"#;

    let files = transpile_files(wat, 1);
    let funcs_0 = files
        .iter()
        .find(|f| f.name == "funcs_0.rs")
        .expect("funcs_0.rs emitted");

    let expected = [
        "#[allow(unused_imports)]",
        "use super::*;",
        "",
        ALLOW,
        "pub fn func0(l0: i32) -> i32 {",
        "    l0",
        "}",
        "",
    ]
    .join("\n");
    assert_eq!(funcs_0.code, expected);
}

/// The first chunk of a stateful two-function module (it declares linear
/// memory), split one function per file. A stateful chunk wraps its functions
/// in an `impl Instance` block, so the method signature sits at four spaces and
/// the body at eight — the indent that a compile-only test cannot verify.
#[test]
fn stateful_chunk_layout_is_byte_exact() {
    let wat = r#"(module
        (memory 1)
        (func (param i32) (result i32) local.get 0)
        (func (result i32) i32.const 7))"#;

    let files = transpile_files(wat, 1);
    let funcs_0 = files
        .iter()
        .find(|f| f.name == "funcs_0.rs")
        .expect("funcs_0.rs emitted");

    let expected = [
        "#[allow(unused_imports)]",
        "use super::*;",
        "",
        ALLOW,
        "impl Instance {",
        "",
        "    pub fn func0(&mut self, l0: i32) -> i32 {",
        "        l0",
        "    }",
        "}",
        "",
    ]
    .join("\n");
    assert_eq!(funcs_0.code, expected);
}
