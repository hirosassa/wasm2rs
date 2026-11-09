//! `wasi:http/types` fields on the typed host, called from a component with the import
//! declared in WIT text: the canonical ABI lowering of strings, byte lists and
//! `result<_, header-error>` reaches the host methods.

use crate::wasip2_typed_host::{assert_succeeded, component_from_wit, run_program};

const HTTP: &str = r#"
package wasi:http@0.2.0;
interface types {
  variant header-error { invalid-syntax, forbidden, immutable }
  resource fields {
    constructor();
    has: func(name: string) -> bool;
    append: func(name: string, value: list<u8>) -> result<_, header-error>;
    delete: func(name: string) -> result<_, header-error>;
  }
}
"#;

const WORLD: &str = r#"
package test:t;
world w {
  import wasi:http/types@0.2.0;
}
"#;

const CORE: &str = r#"
(module
  (import "wasi:http/types@0.2.0" "[resource-drop]fields" (func $drop (param i32)))
  (import "wasi:http/types@0.2.0" "[constructor]fields" (func $new (result i32)))
  (import "wasi:http/types@0.2.0" "[method]fields.has" (func $has (param i32 i32 i32) (result i32)))
  (import "wasi:http/types@0.2.0" "[method]fields.append" (func $append (param i32 i32 i32 i32 i32 i32)))
  (import "wasi:http/types@0.2.0" "[method]fields.delete" (func $delete (param i32 i32 i32 i32)))
  (memory (export "memory") 1)
  (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32) (i32.const 0))
  (data (i32.const 200) "Accept")
  (data (i32.const 210) "a")
  (data (i32.const 220) "host")
  ;; append "Accept: a", then look the name up in another case: 1 if found
  (func (export "found") (result i32) (local $f i32)
    (local.set $f (call $new))
    (call $append (local.get $f) (i32.const 200) (i32.const 6) (i32.const 210) (i32.const 1) (i32.const 100))
    (if (i32.load8_u (i32.const 100)) (then unreachable))
    (call $has (local.get $f) (i32.const 200) (i32.const 6)))
  ;; the result of appending a forbidden header: 1000 + the error case (0 = ok)
  (func (export "forbidden") (result i32) (local $f i32)
    (local.set $f (call $new))
    (call $append (local.get $f) (i32.const 220) (i32.const 4) (i32.const 210) (i32.const 1) (i32.const 100))
    (i32.add (i32.mul (i32.load8_u (i32.const 100)) (i32.const 1000))
             (i32.load8_u (i32.const 101))))
  ;; delete after append leaves the name absent
  (func (export "deleted") (result i32) (local $f i32)
    (local.set $f (call $new))
    (call $append (local.get $f) (i32.const 200) (i32.const 6) (i32.const 210) (i32.const 1) (i32.const 100))
    (call $delete (local.get $f) (i32.const 200) (i32.const 6) (i32.const 100))
    (call $drop (local.get $f))
    (i32.load8_u (i32.const 100))))
"#;

fn run(body: &str) -> std::process::Output {
    run_program(
        &component_from_wit(&[HTTP], WORLD, "w"),
        CORE,
        "WasiHost::new(Vec::new())",
        body,
    )
}

#[test]
fn an_appended_header_is_found_by_name() {
    assert_succeeded(&run("assert_eq!(inst.found(), 1);"));
}

#[test]
fn a_forbidden_header_returns_the_forbidden_case() {
    // result::err (1) with header-error::forbidden (1)
    assert_succeeded(&run("assert_eq!(inst.forbidden(), 1001);"));
}

#[test]
fn delete_succeeds_on_an_existing_header() {
    assert_succeeded(&run("assert_eq!(inst.deleted(), 0);"));
}
