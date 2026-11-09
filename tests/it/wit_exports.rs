//! `wit_exports`: the `canon lift` side of a component, resolved to core module exports.

use wasm2rs::{WitType, wit_exports};
use wit_component::{ComponentEncoder, StringEncoding, embed_component_metadata};
use wit_parser::Resolve;

const RUN_WIT: &str = "package wasi:cli@0.2.0;
interface run { run: func() -> result; }
world w { export run; }";

const RUN_CORE: &str = r#"(module
  (func (export "wasi:cli/run@0.2.0#run") (result i32) i32.const 0))"#;

const STRINGS_WIT: &str = "package t:p;
interface i { f: func(s: string, l: list<u32>) -> string; g: func(); }
world w { export i; export h: func(x: u32) -> u32; }";

const STRINGS_CORE: &str = r#"(module
  (memory (export "memory") 1)
  (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32) i32.const 0)
  (func (export "t:p/i#f") (param i32 i32 i32 i32) (result i32) i32.const 0)
  (func (export "cabi_post_t:p/i#f") (param i32))
  (func (export "t:p/i#g"))
  (func (export "h") (param i32) (result i32) local.get 0))"#;

/// A component of `world` in `wit`, around the core module `core`, from `wit-component`.
fn encoded(wit: &str, core: &str) -> Vec<u8> {
    let mut resolve = Resolve::default();
    let package = resolve.push_str("test.wit", wit).unwrap();
    let world = resolve.select_world(&[package], Some("w")).unwrap();
    let mut module = wat::parse_str(core).unwrap();
    embed_component_metadata(&mut module, &resolve, world, StringEncoding::UTF8, false).unwrap();
    ComponentEncoder::default()
        .module(&module)
        .unwrap()
        .validate(true)
        .encode()
        .unwrap()
}

fn exports_of(wat: &str) -> Result<Vec<wasm2rs::WitExport>, wasm2rs::TranspileError> {
    let component =
        wat::parse_str(wat).map_err(|e| wasm2rs::TranspileError::Unsupported(e.to_string()));
    wit_exports(&component?)
}

fn rejection(wat: &str) -> String {
    exports_of(wat).map(|_| ()).unwrap_err().to_string()
}

#[test]
fn a_cli_run_export_resolves_to_its_core_export() {
    let exports = wit_exports(&encoded(RUN_WIT, RUN_CORE)).unwrap();
    assert_eq!(exports.len(), 1);
    let run = &exports[0];
    assert_eq!(run.func.interface, "wasi:cli/run@0.2.0");
    assert_eq!(run.func.name, "run");
    assert!(run.func.params.is_empty());
    assert!(matches!(run.func.result, Some(WitType::Result { .. })));
    assert_eq!(run.core_module, 0);
    assert_eq!(run.core_export, "wasi:cli/run@0.2.0#run");
    assert_eq!(run.memory, None);
    assert_eq!(run.realloc, None);
    assert_eq!(run.post_return, None);
    assert!(run.utf8);
}

#[test]
fn string_and_list_exports_carry_their_lift_options() {
    let exports = wit_exports(&encoded(STRINGS_WIT, STRINGS_CORE)).unwrap();
    let by_name = |interface: &str, name: &str| {
        exports
            .iter()
            .find(|e| e.func.interface == interface && e.func.name == name)
            .unwrap()
    };
    let f = by_name("t:p/i", "f");
    assert_eq!(f.core_export, "t:p/i#f");
    assert_eq!(f.memory.as_deref(), Some("memory"));
    assert_eq!(f.realloc.as_deref(), Some("cabi_realloc"));
    assert_eq!(f.post_return.as_deref(), Some("cabi_post_t:p/i#f"));
    assert_eq!(
        f.func
            .params
            .iter()
            .map(|p| p.0.as_str())
            .collect::<Vec<_>>(),
        ["s", "l"]
    );
    assert!(matches!(f.func.result, Some(WitType::String)));

    let g = by_name("t:p/i", "g");
    assert_eq!(g.core_export, "t:p/i#g");
    assert_eq!(
        (g.realloc.as_deref(), g.post_return.as_deref()),
        (None, None)
    );

    // A function exported at the top level has no interface.
    let h = by_name("", "h");
    assert_eq!(h.core_export, "h");
    assert_eq!(exports.len(), 3);
}

/// Two top-level core modules; the lifted functions come from the second one.
const TWO_MODULES: &str = r#"(component
  (core module $a (func (export "unused")))
  (core module $b
    (memory (export "mem") 1)
    (func (export "alloc") (param i32 i32 i32 i32) (result i32) i32.const 0)
    (func (export "impl") (param i32 i32) (result i32) i32.const 0)
    (func (export "post") (param i32)))
  (core instance $ia (instantiate $a))
  (core instance $ib (instantiate $b))
  (alias core export $ib "mem" (core memory $m))
  (alias core export $ib "alloc" (core func $r))
  (alias core export $ib "impl" (core func $f))
  (alias core export $ib "post" (core func $p))
  (type $t (func (param "s" string) (result string)))
  (func $lf (type $t) (canon lift (core func $f) (memory $m) (realloc $r) (post-return $p)))
  (instance $i (export "greet" (func $lf)))
  (export "a:b/c" (instance $i))
  (export "top" (func $lf)))"#;

#[test]
fn the_core_module_is_its_index_among_the_top_level_modules() {
    let exports = exports_of(TWO_MODULES).unwrap();
    assert_eq!(exports.len(), 2);
    for e in &exports {
        assert_eq!(e.core_module, 1);
        assert_eq!(e.core_export, "impl");
        assert_eq!(e.memory.as_deref(), Some("mem"));
        assert_eq!(e.realloc.as_deref(), Some("alloc"));
        assert_eq!(e.post_return.as_deref(), Some("post"));
    }
    assert_eq!(
        (
            exports[0].func.interface.as_str(),
            exports[0].func.name.as_str()
        ),
        ("a:b/c", "greet")
    );
    assert_eq!(
        (
            exports[1].func.interface.as_str(),
            exports[1].func.name.as_str()
        ),
        ("", "top")
    );
}

#[test]
fn a_component_without_exports_has_none() {
    assert!(exports_of("(component)").unwrap().is_empty());
}

#[test]
fn a_core_module_is_not_a_component() {
    let wasm = wat::parse_str("(module)").unwrap();
    assert!(wit_exports(&wasm).is_err());
}

#[test]
fn a_core_function_from_an_exports_only_instance_is_rejected() {
    let err = rejection(
        r#"(component
  (core module $b (func (export "f")))
  (core instance $ib (instantiate $b))
  (alias core export $ib "f" (core func $f0))
  (core instance $shim (export "f" (func $f0)))
  (alias core export $shim "f" (core func $f))
  (type $t (func))
  (func $lf (type $t) (canon lift (core func $f)))
  (export "go" (func $lf)))"#,
    );
    assert!(err.contains("`go`") && err.contains("shim"), "{err}");
}

#[test]
fn a_core_function_that_is_a_canon_lower_is_rejected() {
    let err = rejection(
        r#"(component
  (import "a:b/i" (instance $ii (export "f" (func))))
  (alias export $ii "f" (func $imp))
  (core func $lo (canon lower (func $imp)))
  (type $t (func))
  (func $lf (type $t) (canon lift (core func $lo)))
  (instance $i (export "f" (func $lf)))
  (export "a:b/c" (instance $i)))"#,
    );
    assert!(
        err.contains("a:b/c#f") && err.contains("canon lower"),
        "{err}"
    );
}

#[test]
fn a_string_encoding_other_than_utf8_is_rejected() {
    for encoding in ["utf16", "latin1+utf16"] {
        let err = rejection(&format!(
            r#"(component
  (core module $b
    (memory (export "mem") 1)
    (func (export "alloc") (param i32 i32 i32 i32) (result i32) i32.const 0)
    (func (export "f") (param i32 i32)))
  (core instance $ib (instantiate $b))
  (alias core export $ib "mem" (core memory $m))
  (alias core export $ib "alloc" (core func $r))
  (alias core export $ib "f" (core func $f))
  (type $t (func (param "s" string)))
  (func $lf (type $t) (canon lift (core func $f) (memory $m) (realloc $r) string-encoding={encoding}))
  (export "say" (func $lf)))"#
        ));
        assert!(
            err.contains("`say`") && err.contains("string encoding"),
            "{encoding}: {err}"
        );
    }
}

#[test]
fn a_lift_option_from_another_core_instance_is_rejected() {
    let err = rejection(
        r#"(component
  (core module $a (func (export "alloc") (param i32 i32 i32 i32) (result i32) i32.const 0))
  (core module $b (memory (export "mem") 1) (func (export "f") (param i32 i32)))
  (core instance $ia (instantiate $a))
  (core instance $ib (instantiate $b))
  (alias core export $ib "mem" (core memory $m))
  (alias core export $ia "alloc" (core func $r))
  (alias core export $ib "f" (core func $f))
  (type $t (func (param "s" string)))
  (func $lf (type $t) (canon lift (core func $f) (memory $m) (realloc $r)))
  (export "say" (func $lf)))"#,
    );
    assert!(err.contains("`say`") && err.contains("realloc"), "{err}");
}

const NESTED: &str = r#"(component $inner
    (core module $m (func (export "f")))
    (core instance $i (instantiate $m))
    (alias core export $i "f" (core func $f))
    (func (export "f") (canon lift (core func $f))))"#;

#[test]
fn an_instance_of_a_nested_component_is_rejected() {
    let err = rejection(&format!(
        r#"(component
  {NESTED}
  (instance $ii (instantiate $inner))
  (export "n" (instance $ii)))"#
    ));
    assert!(
        err.contains("`n`") && err.contains("nested component"),
        "{err}"
    );
}

#[test]
fn a_function_aliased_from_a_nested_component_is_rejected() {
    let err = rejection(&format!(
        r#"(component
  {NESTED}
  (instance $ii (instantiate $inner))
  (alias export $ii "f" (func $g))
  (export "g" (func $g)))"#
    ));
    assert!(
        err.contains("`g`") && err.contains("nested component"),
        "{err}"
    );
}

#[test]
fn malformed_input_is_an_error_and_never_a_panic() {
    for component in [
        encoded(RUN_WIT, RUN_CORE),
        encoded(STRINGS_WIT, STRINGS_CORE),
        wat::parse_str(TWO_MODULES).unwrap(),
    ] {
        // A cut at a section boundary is a valid (smaller) component, so only the absence
        // of a panic is checked for every prefix; a cut inside the last section is an error.
        for len in 0..component.len() {
            let _ = wit_exports(&component[..len]);
        }
        assert!(wit_exports(&component[..component.len() - 1]).is_err());
        // A flipped byte may still be a valid component; it must not panic either way.
        for at in 0..component.len() {
            let mut bad = component.clone();
            bad[at] ^= 0xff;
            let _ = wit_exports(&bad);
        }
    }
}

#[test]
fn core_module_counts_only_the_top_level_modules() {
    // Re-exporting a module grows the module index space but is not a new core module.
    let exports = exports_of(
        r#"(component
  (core module $a (func (export "f")))
  (export "m" (core module $a))
  (core module $b (func (export "f")))
  (core instance $ib (instantiate 2))
  (alias core export $ib "f" (core func $f))
  (type $t (func))
  (func $lf (type $t) (canon lift (core func $f)))
  (export "go" (func $lf)))"#,
    )
    .unwrap();
    assert_eq!(exports[0].core_module, 1);
}

#[test]
fn a_module_aliased_from_an_instance_is_not_taken_for_a_top_level_module() {
    let err = rejection(
        r#"(component
  (import "x" (instance $x (export "m" (core module (export "f" (func))))))
  (alias export $x "m" (core module $im))
  (core module $a (func (export "f")))
  (core instance $i (instantiate $im))
  (alias core export $i "f" (core func $f))
  (type $t (func))
  (func $lf (type $t) (canon lift (core func $f)))
  (export "top" (func $lf)))"#,
    );
    assert!(
        err.contains("`top`") && err.contains("core module"),
        "{err}"
    );
}
