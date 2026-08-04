//! Tests that operands consumed in place by a boundary operation are inlined
//! into the emitted statement instead of being spilled to a throwaway `let`.
//!
//! A read of a *mutable* local is not "stable" (its value can change), so the
//! generator freezes non-stable operands before a boundary that could observe
//! or cause a mutation. But an operand the boundary consumes *right now* is
//! placed into the emitted expression in program order, so freezing it into a
//! temporary is redundant: `local.set 1 (local.get 0)` should render as
//! `l1 = l0;`, not `let v0 = l0; l1 = v0;`. Behaviour must stay identical, so
//! each shape assertion is paired with a `compile_run` that checks the value.

mod common;

use common::compile_run;

/// Transpile `wat` to Rust source for shape assertions.
fn transpile(wat: &str) -> String {
    let wasm = wat::parse_str(wat).expect("valid wat");
    wasm2rs::transpile(&wasm).expect("transpile ok")
}

#[test]
fn local_set_inlines_a_mutable_local_read() {
    // `l0` is mutable (it is assigned), so reading it is non-stable. The read is
    // consumed immediately by `local.set 1`, so it must inline: `l1 = l0;`.
    let wat = r#"(module (func (export "f") (result i32)
        (local i32) (local i32)
        (local.set 0 (i32.const 5))
        (local.set 1 (local.get 0))
        (local.get 1)))"#;
    let src = transpile(wat);

    assert!(
        src.contains("l1 = l0;"),
        "expected the consumed local read to inline as `l1 = l0;`\n{src}",
    );
    assert!(
        !src.contains(": i32 = l0;"),
        "the mutable local read should not be spilled to a `let` temporary\n{src}",
    );

    compile_run("inline_local_set", wat, "assert_eq!(func0(), 5);");
}

#[test]
fn global_set_inlines_a_mutable_global_read() {
    // Reading mutable global `g0` is non-stable; consumed by `global.set 1` it
    // should inline into `self.g1 = self.g0;` rather than via a temporary.
    let wat = r#"(module
        (global (mut i32) (i32.const 7))
        (global (mut i32) (i32.const 0))
        (func (export "f") (result i32)
            (global.set 1 (global.get 0))
            (global.get 1)))"#;
    let src = transpile(wat);

    assert!(
        src.contains("self.g1 = self.g0;"),
        "expected the consumed global read to inline as `self.g1 = self.g0;`\n{src}",
    );

    compile_run(
        "inline_global_set",
        wat,
        "let mut inst = Instance::new();\n    assert_eq!(inst.func0(), 7);",
    );
}

#[test]
fn store_inlines_the_consumed_address_and_value() {
    // `i32.store` consumes both the address and the value here, so a non-stable
    // value (read of mutable `l1`) inlines into the store call.
    let wat = r#"(module (memory 1) (func (export "f") (result i32)
        (local i32)
        (local.set 0 (i32.const 42))
        (i32.store (i32.const 0) (local.get 0))
        (i32.load (i32.const 0))))"#;
    let src = transpile(wat);

    assert!(
        src.contains(", l0);"),
        "expected the consumed store value to inline as the store's argument\n{src}",
    );
    assert!(
        !src.contains(": i32 = l0;"),
        "the mutable local read should not be spilled before the store\n{src}",
    );

    compile_run(
        "inline_store",
        wat,
        "let mut inst = Instance::new();\n    assert_eq!(inst.func0(), 42);",
    );
}

#[test]
fn call_inlines_a_consumed_argument() {
    // The call argument (read of mutable `l1`) is consumed in place, so it
    // inlines into the call expression rather than via a temporary. The call
    // *result* is still materialised — a call is not re-evaluatable.
    let wat = r#"(module
        (func $g (param i32) (result i32) (local.get 0))
        (func (export "f") (result i32)
            (local i32)
            (local.set 0 (i32.const 9))
            (call $g (local.get 0))))"#;
    let src = transpile(wat);

    assert!(
        src.contains("func0(l0)"),
        "expected the consumed call argument to inline as `func0(l0)`\n{src}",
    );
    assert!(
        !src.contains(": i32 = l0;"),
        "the mutable local read should not be spilled before the call\n{src}",
    );

    compile_run("inline_call", wat, "assert_eq!(func1(), 9);");
}

#[test]
fn call_indirect_inlines_the_index_but_keeps_arguments_spilled() {
    // The table index inlines into the entry lookup, but the arguments must stay
    // spilled to temporaries: the entry lookup is emitted before the dispatch
    // call, so keeping the args as temps preserves wasm evaluation order
    // (arguments are evaluated before the index).
    let wat = r#"(module
        (type $t (func (param i32) (result i32)))
        (table 1 funcref)
        (func $g (param i32) (result i32) (local.get 0))
        (elem (i32.const 0) $g)
        (func (export "f") (result i32)
            (local i32) (local i32)
            (local.set 0 (i32.const 5))
            (local.set 1 (i32.const 0))
            (call_indirect (type $t) (local.get 0) (local.get 1))))"#;
    let src = transpile(wat);

    assert!(
        src.contains("self.table()[(l1) as u32 as usize]"),
        "expected the consumed table index to inline into the entry lookup\n{src}",
    );
    assert!(
        src.contains(": i32 = l0;"),
        "the argument must stay spilled so it is evaluated before the index\n{src}",
    );

    compile_run(
        "inline_call_indirect",
        wat,
        "let mut inst = Instance::new();\n    assert_eq!(inst.func1(), 5);",
    );
}
