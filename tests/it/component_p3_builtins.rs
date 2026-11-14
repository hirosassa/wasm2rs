//! WASI 0.3 canonical built-ins (`task.return`, `context.*`, `stream.*`, ...) imported by the
//! program core module are identified, and a built-in the glue cannot serve (it needs the M2
//! runtime) is an `Unsupported` error naming its feature instead of an unknown import.

use wasm2rs::{ComponentOptions, TranspileError, transpile_component};

/// An async export `f` (callback ABI) over a core module that imports `builtin` (given as the
/// `canon` text of a core func and the core signature the module imports it with).
fn guest(canon: &str, import_sig: &str) -> Vec<u8> {
    wat::parse_str(format!(
        r#"(component
  (core module $m
    (import "" "b" (func {import_sig}))
    (func (export "f") (result i32) i32.const 0)
    (func (export "cb") (param i32 i32 i32) (result i32) i32.const 0))
  (core func $b (canon {canon}))
  (core instance $i (export "b" (func $b)))
  (core instance $mi (instantiate $m (with "" (instance $i))))
  (func (export "f") async (result u32)
    (canon lift (core func $mi "f") async (callback (core func $mi "cb")))))"#
    ))
    .unwrap()
}

fn unsupported(canon: &str, import_sig: &str) -> String {
    let err = transpile_component(&guest(canon, import_sig), &ComponentOptions::default())
        .expect_err("a built-in import is refused");
    assert!(matches!(err, TranspileError::Unsupported(_)), "{err:?}");
    err.to_string()
}

#[test]
fn task_return_is_named() {
    let what = unsupported("task.return (result u32)", "(param i32)");
    assert!(what.contains("task.return"), "{what}");
}

#[test]
fn context_get_is_named() {
    let what = unsupported("context.get i32 0", "(result i32)");
    assert!(what.contains("context.get"), "{what}");
}

#[test]
fn waitable_set_new_is_named() {
    let what = unsupported("waitable-set.new", "(result i32)");
    assert!(what.contains("waitable-set"), "{what}");
}

#[test]
fn subtask_drop_is_named() {
    let what = unsupported("subtask.drop", "(param i32)");
    assert!(what.contains("subtask"), "{what}");
}
