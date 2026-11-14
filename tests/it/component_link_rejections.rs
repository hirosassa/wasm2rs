//! Rejection contracts of the component linker: shapes of a linked core module that the
//! linker cannot fold into one unit must fail with a named reason, never produce wrong code.

use wasm2rs::{ComponentOptions, TranspileError, transpile_component};

/// A component with a program module `$main` (memory, a tag, a lifted `peek`) and a library
/// `$lib` with `lib_body` as its items, instantiated against `main`'s memory and tag.
fn with_library(lib_body: &str) -> Vec<u8> {
    let wat = format!(
        r#"(component
    (core module $main
        (memory (export "memory") 1)
        (tag $t (export "tag"))
        (global (export "gf") funcref (ref.null func))
        (table (export "table") 4 funcref)
        (func (export "peek") (param i32) (result i32) (i32.load8_u (local.get 0))))
    (core module $lib
        (import "env" "memory" (memory 1))
        {lib_body}
        (func (export "nop") (result i32) (i32.const 0)))
    (core instance $m (instantiate $main))
    (alias core export $m "memory" (core memory $mem))
    (alias core export $m "tag" (core tag $tag))
    (alias core export $m "gf" (core global $gf))
    (alias core export $m "table" (core table $tbl))
    (core instance $env (export "memory" (memory $mem)) (export "tag" (tag $tag))
        (export "gf" (global $gf)) (export "table" (table $tbl)))
    (core instance $l (instantiate $lib (with "env" (instance $env))))
    (func $peek (param "a" s32) (result s32) (canon lift (core func $m "peek")))
    (export "peek" (func $peek))
    (func $nop (result s32) (canon lift (core func $l "nop")))
    (export "nop" (func $nop)))"#
    );
    wat::parse_str(wat).unwrap()
}

/// Transpiles a component whose library has `lib_body` and asserts it is refused as
/// `Unsupported` with a message naming `reason`.
fn assert_refused(lib_body: &str, reason: &str) {
    let err =
        transpile_component(&with_library(lib_body), &ComponentOptions::default()).unwrap_err();
    assert!(matches!(err, TranspileError::Unsupported(_)), "{err:?}");
    let why = err.to_string();
    assert!(why.contains(reason), "{why}");
}

#[test]
fn a_librarys_global_initializer_outside_the_foldable_forms_is_refused() {
    assert_refused(
        r#"(global v128 (v128.const i32x4 1 2 3 4))"#,
        "in a global initializer of a linked module",
    );
}

#[test]
fn a_librarys_exception_tag_import_is_refused() {
    assert_refused(r#"(import "env" "tag" (tag))"#, "exception tag import");
}

#[test]
fn a_librarys_table_initializer_reading_an_imported_global_is_refused() {
    assert_refused(
        r#"(import "env" "gf" (global $gf funcref))
        (table 2 funcref (global.get $gf))"#,
        "table initializer of linked module",
    );
}

#[test]
fn a_librarys_element_item_reading_an_imported_global_is_refused() {
    assert_refused(
        r#"(import "env" "gf" (global $gf funcref))
        (import "env" "table" (table 4 funcref))
        (elem (i32.const 0) funcref (global.get $gf))"#,
        "element segment of linked module",
    );
}

/// Control: the shared skeleton itself links, so the refusals above come from the library body.
#[test]
fn the_skeleton_with_a_benign_library_links() {
    let code = transpile_component(
        &with_library("(global i32 (i32.const 1))"),
        &ComponentOptions::default(),
    )
    .unwrap();
    assert!(code.contains("pub fn peek("), "{code}");
}

#[test]
fn a_custom_page_size_memory_in_a_component_module_is_refused() {
    let wat = r#"(component
        (core module $m (memory (export "memory") 1 (pagesize 1))
            (func (export "f") (result i32) (i32.const 0)))
        (core instance $i (instantiate $m))
        (func $f (result s32) (canon lift (core func $i "f")))
        (export "f" (func $f)))"#;
    let err = transpile_component(&wat::parse_str(wat).unwrap(), &ComponentOptions::default())
        .unwrap_err();
    assert!(
        matches!(err.root(), TranspileError::Unsupported(_)),
        "{err:?}"
    );
    assert!(err.to_string().contains("custom page sizes"), "{err}");
}
