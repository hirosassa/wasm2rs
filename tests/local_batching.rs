//! Tests for batching zero-initialised local declarations.
//!
//! Declared (non-parameter) locals all default to zero, so consecutive locals
//! of the same Rust type are collapsed into a single tuple declaration —
//! `let (mut l1, mut l2): (i32, i32) = (0, 0);` — instead of one `let` line
//! each. `mut` is placed only on the locals that are actually mutated, a single
//! local of a type stays a plain scalar binding, and different types form
//! separate tuples.

mod common;

use common::compile_run;

fn transpile(wat: &str) -> String {
    let wasm = wat::parse_str(wat).expect("valid wat");
    wasm2rs::transpile(&wasm).expect("transpile ok")
}

#[test]
fn same_type_mutated_locals_are_batched() {
    let wat = r#"(module (func (export "f") (param i32) (result i32)
        (local i32) (local i32) (local i64)
        (local.set 1 (i32.const 5))
        (local.set 2 (i32.const 7))
        (local.set 3 (i64.const 9))
        (i32.add (local.get 1) (local.get 2))))"#;
    let src = transpile(wat);

    assert!(
        src.contains("let (mut l1, mut l2): (i32, i32) = (0, 0);"),
        "two mutated i32 locals should batch into one tuple decl\n{src}",
    );
    // The lone i64 local stays a scalar binding rather than a 1-tuple.
    assert!(
        src.contains("let mut l3: i64 = 0;"),
        "a single local of a type should remain a scalar binding\n{src}",
    );
    // The old one-line-per-local form for the batched locals is gone.
    assert!(
        !src.contains("let mut l1: i32 = 0;"),
        "the batched locals should not also appear as scalar decls\n{src}",
    );

    compile_run("local_batch_mut", wat, "assert_eq!(func0(0), 12);");
}

#[test]
fn unmutated_locals_batch_without_mut() {
    // Locals that are only read (never `local.set`) are immutable, so the tuple
    // binding carries no `mut`.
    let wat = r#"(module (func (export "f") (result i32)
        (local i32) (local i32)
        (i32.add (local.get 0) (local.get 1))))"#;
    let src = transpile(wat);

    assert!(
        src.contains("let (l0, l1): (i32, i32) = (0, 0);"),
        "two read-only i32 locals should batch without `mut`\n{src}",
    );

    compile_run("local_batch_readonly", wat, "assert_eq!(func0(), 0);");
}

#[test]
fn mixed_mutability_places_mut_per_local() {
    // Same type, but only one local is mutated: `mut` goes on that one only.
    let wat = r#"(module (func (export "f") (result i32)
        (local i32) (local i32)
        (local.set 0 (i32.const 3))
        (i32.add (local.get 0) (local.get 1))))"#;
    let src = transpile(wat);

    assert!(
        src.contains("let (mut l0, l1): (i32, i32) = (0, 0);"),
        "mut should be placed only on the mutated local\n{src}",
    );

    compile_run("local_batch_mixed", wat, "assert_eq!(func0(), 3);");
}
