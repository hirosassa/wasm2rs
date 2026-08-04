//! Tests for the memory-helper method names and the address argument form.
//!
//! The load/store helpers are the most frequently called methods in generated
//! code, so their names are terse: `r*` (read/load) and `w*` (write/store).
//! And because a wasm address is already an `i32` on the operand stack, the
//! helpers take `addr: i32` and cast internally, so a call passes the address
//! expression directly instead of wrapping it as `(expr) as u32`.

mod common;

use common::{compile_run, expect_trap};

fn transpile(wat: &str) -> String {
    let wasm = wat::parse_str(wat).expect("valid wat");
    wasm2rs::transpile(&wasm).expect("transpile ok")
}

#[test]
fn load_store_helpers_use_terse_names() {
    let wat = r#"(module (memory 1) (func (export "f") (param i32) (result i32)
        (i32.store (local.get 0) (i32.const 42))
        (i32.load (local.get 0))))"#;
    let src = transpile(wat);

    assert!(
        src.contains("self.w32(") && src.contains("self.r32("),
        "expected terse helper names `r32`/`w32`\n{src}",
    );
    assert!(
        !src.contains("load_i32") && !src.contains("store_i32"),
        "the old verbose helper names should be gone\n{src}",
    );

    compile_run(
        "helper_names",
        wat,
        "let mut inst = Instance::new();\n    assert_eq!(inst.func0(16), 42);",
    );
}

#[test]
fn load_store_pass_the_address_without_a_cast() {
    // The address expression is an i32 already, so it is passed straight into
    // the helper (which casts internally) rather than wrapped as `(expr) as u32`.
    let wat = r#"(module (memory 1) (func (export "f") (param i32) (result i32)
        (i32.store (local.get 0) (i32.const 42))
        (i32.load (local.get 0))))"#;
    let src = transpile(wat);

    assert!(
        src.contains("self.w32(l0, 0u32, 42i32)") && src.contains("self.r32(l0, 0u32)"),
        "expected the address to be passed without an `as u32` cast\n{src}",
    );
    assert!(
        !src.contains("(l0) as u32"),
        "the address should not be cast at the call site\n{src}",
    );

    compile_run(
        "helper_addr_no_cast",
        wat,
        "let mut inst = Instance::new();\n    assert_eq!(inst.func0(16), 42);",
    );
}

#[test]
fn negative_address_is_treated_as_unsigned_and_traps() {
    // A wasm address is unsigned: the i32 `-1` denotes 0xFFFF_FFFF, which is far
    // out of a one-page memory, so it must trap — not index memory negatively.
    let wat = r#"(module (memory 1) (func (export "f") (param i32)
        (i32.store (local.get 0) (i32.const 1))))"#;

    expect_trap(
        "helper_addr_unsigned",
        wat,
        "let mut inst = Instance::new();\n    inst.func0(-1);",
    );
}
