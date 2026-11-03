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

use common::transpile_files;

const ALLOW: &str = "#[allow(dead_code, unused_variables, unused_assignments, unused_mut, unused_parens, unconditional_recursion)]";

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

/// A stateful single-file module: the `impl Instance` block holds every
/// function at one indent level, separated by one blank line, and the module
/// tail (the closing brace and the external-funcref registry) follows the last
/// function. Pins the single-file layout while its assembly is restructured.
#[test]
fn stateful_single_file_layout_is_stable() {
    let wat = r#"(module
        (import "env" "host" (func $host (param i32) (result i32)))
        (memory 1)
        (table 2 funcref)
        (elem (i32.const 0) $a $b)
        (type $t (func (param i32) (result i32)))
        (func $a (type $t) (call $host (local.get 0)))
        (func $b (type $t)
          (local i32)
          (loop $l
            (local.set 1 (i32.add (local.get 1) (i32.const 1)))
            (br_if $l (i32.lt_u (local.get 1) (local.get 0))))
          (i32.load offset=4 (local.get 1)))
        (func (export "run") (param i32) (result i32)
          (call_indirect (type $t) (local.get 0) (i32.const 1))))"#;
    let wasm = wat::parse_str(wat).unwrap();
    let src = wasm2rs::transpile(&wasm).unwrap();

    let start = src.find("    pub fn func1(").expect("func1 method");
    let region = &src[start..];
    let end = region.find("\n}\n").expect("closing brace of the impl");
    let funcs = &region[..end];

    // Every non-empty line of the function region is indented one level.
    for line in funcs.lines().filter(|l| !l.is_empty()) {
        assert!(line.starts_with("    "), "unindented line {line:?}\n{src}");
    }
    // Functions are separated by exactly one blank line, never two.
    assert!(!funcs.contains("\n\n\n"), "double blank line\n{src}");
    for name in ["func1", "func2"] {
        let needle = format!("    pub fn {name}(");
        let at = src.find(&needle).expect(&needle);
        // A blank line (and nothing else) separates a method from what precedes it.
        assert!(
            src[..at].ends_with("}\n\n") || src[..at].ends_with("\n\n"),
            "{name} is not blank-line separated\n{src}"
        );
    }
    // The tail: the impl closes right after the last function and the file
    // ends with a newline.
    assert!(src.ends_with("}\n"), "{src}");
    assert!(region[end..].starts_with("\n}\n"), "{src}");
}
