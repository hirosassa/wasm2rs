//! Tests for the memory-helper method names and the address argument form.
//!
//! The load/store helpers are the most frequently called methods in generated
//! code, so their names are terse: `r*` (read/load) and `w*` (write/store).
//! And because a wasm address is already an `i32` on the operand stack, the
//! helpers take `addr: i32` and cast internally, so a call passes the address
//! expression directly instead of wrapping it as `(expr) as u32`.

use crate::common;

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
        src.contains("self.w32(l0, 0, 42i32)") && src.contains("self.r32(l0, 0)"),
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

#[test]
fn static_offsets_are_passed_without_a_type_suffix() {
    // The helper's `offset` parameter fixes the literal's type, so a `u32`
    // suffix is redundant. A non-zero offset checks the value is still passed.
    let wat = r#"(module (memory 1) (data (i32.const 40) "\2a\00\00\00")
        (func (export "f") (param i32) (result i32)
          (i32.store offset=8 (local.get 0) (i32.const 7))
          (i32.add
            (i32.load offset=8 (local.get 0))
            (i32.load offset=40 (i32.const 0)))))"#;
    let src = transpile(wat);

    assert!(
        src.contains("self.r32(l0, 8)") && src.contains("self.w32(l0, 8, 7i32)"),
        "expected unsuffixed offsets\n{src}",
    );
    assert!(
        !src.contains("8u32") && !src.contains("0u32"),
        "no offset should carry a `u32` suffix\n{src}",
    );

    compile_run(
        "helper_offset_no_suffix",
        wat,
        "let mut inst = Instance::new();\n    assert_eq!(inst.func0(16), 49);",
    );
}

#[test]
fn memory64_offsets_are_passed_without_a_type_suffix() {
    let wat = r#"(module (memory i64 1)
        (func (export "f") (param i64) (result i32)
          (i32.store offset=8 (local.get 0) (i32.const 7))
          (i32.load offset=8 (local.get 0))))"#;
    let src = transpile(wat);

    assert!(
        src.contains("self.r32(l0, 8)") && !src.contains("8u64"),
        "expected an unsuffixed memory64 offset\n{src}",
    );

    compile_run(
        "helper_offset64_no_suffix",
        wat,
        "let mut inst = Instance::new();\n    assert_eq!(inst.func0(16), 7);",
    );
}
