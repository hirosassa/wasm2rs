//! `wit_exports`: the `canon lift` side of a component, resolved to core module exports.

use wasm2rs::{WitStringEncoding, WitType, wit_exports};
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
    assert_eq!(run.encoding, WitStringEncoding::Utf8);
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

/// A component lifting `say` (a string parameter) with `encoding` as its `string-encoding`.
fn say_with(encoding: &str) -> String {
    format!(
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
    )
}

#[test]
fn a_utf16_lift_is_resolved_with_its_encoding() {
    let exports = exports_of(&say_with("utf16")).unwrap();
    assert_eq!(exports[0].encoding, WitStringEncoding::Utf16);
}

#[test]
fn compact_utf16_is_rejected_naming_the_export() {
    let err = rejection(&say_with("latin1+utf16"));
    assert!(
        err.contains("`say`") && err.contains("compact-utf16"),
        "{err}"
    );
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

/// A component that lifts core module `$a`'s export `f` as `$lf`, passes it through the
/// nested component `$shim` (which re-exports its import `f`, with `shim_body` in between)
/// and exports the shim's instance as `i`. `before` comes first, to shift the index spaces.
fn exported_shim_instance_with(before: &str, shim_body: &str) -> String {
    format!(
        r#"(component
  {before}
  (core module $a (func (export "f")))
  (core instance $i (instantiate $a))
  (alias core export $i "f" (core func $f))
  (func $lf (canon lift (core func $f)))
  (component $shim
    (import "f" (func $f))
    {shim_body}
    (export "f" (func $f)))
  (instance $s (instantiate $shim (with "f" (func $lf))))
  (export "i" (instance $s)))"#
    )
}

/// Asserts that, with a shim that only re-exports its import, the one export is `i`'s `f`.
fn assert_exports_the_shim_instance(before: &str) {
    let exports = exports_of(&exported_shim_instance_with(before, "")).unwrap();
    assert_eq!(exports.len(), 1);
    assert_eq!(
        (
            exports[0].func.interface.as_str(),
            exports[0].func.name.as_str()
        ),
        ("i", "f")
    );
}

#[test]
fn an_aliased_instance_takes_a_slot_of_the_instance_index_space() {
    assert_exports_the_shim_instance(
        r#"(import "x" (instance $x (export "n" (instance))))
  (alias export $x "n" (instance $n))"#,
    );
}

#[test]
fn an_aliased_component_takes_a_slot_of_the_component_index_space() {
    // Components are [aliased, $shim]: the instantiation of index 1 must still be the shim.
    assert_exports_the_shim_instance(
        r#"(import "x" (instance $x (export "c" (component))))
  (alias export $x "c" (component $c))"#,
    );
}

#[test]
fn an_instance_of_an_aliased_component_is_never_traced() {
    // Nothing is known about what an aliased component defines.
    let err = rejection(
        r#"(component
  (import "x" (instance $x (export "c" (component))))
  (alias export $x "c" (component $c))
  (instance $ci (instantiate $c))
  (export "n" (instance $ci)))"#,
    );
    assert!(
        err.contains("`n`") && err.contains("nested component"),
        "{err}"
    );
}

#[test]
fn an_outer_alias_of_a_component_takes_a_slot_of_the_component_index_space() {
    // Components are [$c, alias of $c, $shim]; `$c` is empty, so the alias instantiates fine.
    assert_exports_the_shim_instance(
        r#"(component $c)
  (alias outer 0 $c (component $c2))"#,
    );
}

#[test]
fn a_shim_that_defines_anything_of_its_own_is_not_a_passthrough() {
    // Each shim re-exports its import `f`, but also defines something (an instance, a core
    // module, a component), so it is not traced as a re-export.
    for body in [
        r#"(instance $i (export "g" (func $f)))"#,
        r#"(core module $m)"#,
        r#"(component $c)"#,
    ] {
        let err = rejection(&exported_shim_instance_with("", body));
        assert!(
            err.contains("`i`") && err.contains("nested component"),
            "{body}: {err}"
        );
    }
}

#[test]
fn a_type_aliased_out_of_a_nested_instance_type_keeps_its_shape() {
    // `wit_exports` never reads the imported functions, so the nested instance is not
    // refused there: its exported record must still resolve for the lifted parameter.
    let wasm = wat::parse_str(
        r#"(component
  (type $it (instance
    (export "inner" (instance
      (type $r (record (field "a" u32)))
      (export "t" (type (eq $r)))))
    (alias export 0 "t" (type $t))
    (export "t2" (type (eq $t)))))
  (import "x" (instance $x (type $it)))
  (alias export $x "t2" (type $t2))
  (core module $a (memory (export "memory") 1) (func (export "f") (param i32)))
  (core instance $i (instantiate $a))
  (alias core export $i "f" (core func $f))
  (type $ft (func (param "p" $t2)))
  (func $lf (type $ft) (canon lift (core func $f)))
  (export "f" (func $lf)))"#,
    )
    .unwrap();
    let exports = wit_exports(&wasm).unwrap();
    let WitType::Record(record) = &exports[0].func.params[0].1 else {
        unreachable!("{:?}", exports[0].func.params);
    };
    assert_eq!(record.fields.len(), 1);
    assert_eq!(record.fields[0].0, "a");
    assert!(matches!(*record.fields[0].1, WitType::U32));
}

/// A component exporting `go` of type `ty`, lifted with `options` from a core function of
/// signature `core_sig`.
fn p3_export(ty: &str, options: &str, core_sig: &str) -> String {
    format!(
        r#"(component
  (core module $b
    (memory (export "mem") 1)
    (func (export "f") {core_sig})
    (func (export "cb") (param i32 i32 i32) (result i32) i32.const 0))
  (core instance $ib (instantiate $b))
  (alias core export $ib "mem" (core memory $m))
  (alias core export $ib "f" (core func $f))
  (alias core export $ib "cb" (core func $cb))
  (type $t {ty})
  (func $lf (type $t) (canon lift (core func $f) {options}))
  (export "go" (func $lf)))"#
    )
}

#[test]
fn an_async_typed_function_lifted_without_the_async_option_is_rejected() {
    let err = rejection(&p3_export("(func async)", "", ""));
    assert!(
        err.contains("`go`") && err.contains("async function (WASI 0.3"),
        "{err}"
    );
    assert!(!err.contains("stackful"), "{err}");
}

#[test]
fn p3_value_types_are_rejected_by_name() {
    for (ty, name) in [
        ("(func (param \"p\" (future u32)))", "future"),
        ("(func (param \"p\" (stream u8)))", "stream"),
        ("(func (param \"p\" error-context))", "error-context"),
    ] {
        let err = rejection(&p3_export(ty, "(memory $m)", "(param i32)"));
        assert!(
            err.contains("`go`") && err.contains(name) && err.contains("WASI 0.3"),
            "{name}: {err}"
        );
    }
}

#[test]
fn a_stackful_async_lift_is_rejected_as_the_stackful_abi() {
    let err = rejection(&p3_export("(func async)", "async", ""));
    assert!(
        err.contains("`go`") && err.contains("stackful") && err.contains("WASI 0.3"),
        "{err}"
    );
}

#[test]
fn a_callback_async_lift_is_rejected_as_the_callback_abi() {
    let err = rejection(&p3_export(
        "(func async)",
        "async (callback $cb)",
        "(result i32) i32.const 0",
    ));
    assert!(
        err.contains("`go`") && err.contains("callback ABI") && err.contains("WASI 0.3"),
        "{err}"
    );
}

/// A component whose core module imports `a:b/i#f` through a `canon lower` with `options`; the
/// function takes `param` (a WIT type).
fn lowering(param: &str, core_params: &str, options: &str) -> Vec<u8> {
    wat::parse_str(format!(
        r#"(component
  (import "a:b/i" (instance $ii (export "f" (func (param "x" {param})))))
  (alias export $ii "f" (func $imp))
  (core module $m
    (import "h" "f" (func (param {core_params})))
    (memory (export "mem") 1)
    (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32) i32.const 0))
  (core module $mem (memory (export "mem") 1)
    (func (export "alloc") (param i32 i32 i32 i32) (result i32) i32.const 0))
  (core instance $im (instantiate $mem))
  (alias core export $im "mem" (core memory $mm))
  (alias core export $im "alloc" (core func $ra))
  (core func $lo (canon lower (func $imp) {options}))
  (core instance $h (export "f" (func $lo)))
  (core instance (instantiate $m (with "h" (instance $h)))))"#
    ))
    .unwrap()
}

fn glue_error(component: &[u8]) -> String {
    wasm2rs::wit_host_glue(component).unwrap_err().to_string()
}

#[test]
fn an_import_lowered_as_compact_utf16_with_a_string_is_rejected_not_read_as_utf8() {
    let err = glue_error(&lowering(
        "string",
        "i32 i32",
        "(memory $mm) (realloc $ra) string-encoding=latin1+utf16",
    ));
    assert!(
        err.contains("a:b/i#f") && err.contains("compact-utf16"),
        "{err}"
    );
}

#[test]
fn an_import_lowered_as_utf16_with_a_string_is_served() {
    let component = lowering(
        "string",
        "i32 i32",
        "(memory $mm) (realloc $ra) string-encoding=utf16",
    );
    let glue = wasm2rs::wit_host_glue(&component).unwrap();
    assert!(glue.contains("glue_abi::string16("), "{glue}");
}

#[test]
fn an_import_lowered_both_as_utf8_and_as_utf16_is_rejected() {
    // The glue has one method per function, so it cannot read the strings both ways.
    let component = wat::parse_str(
        r#"(component
  (import "a:b/i" (instance $ii (export "f" (func (param "x" string)))))
  (alias export $ii "f" (func $imp))
  (core module $m
    (import "h" "f" (func (param i32 i32)))
    (import "h" "g" (func (param i32 i32)))
    (memory (export "mem") 1)
    (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32) i32.const 0))
  (core module $mem (memory (export "mem") 1)
    (func (export "alloc") (param i32 i32 i32 i32) (result i32) i32.const 0))
  (core instance $im (instantiate $mem))
  (alias core export $im "mem" (core memory $mm))
  (alias core export $im "alloc" (core func $ra))
  (core func $u8 (canon lower (func $imp) (memory $mm) (realloc $ra) string-encoding=utf8))
  (core func $u16 (canon lower (func $imp) (memory $mm) (realloc $ra) string-encoding=utf16))
  (core instance $h (export "f" (func $u8)) (export "g" (func $u16)))
  (core instance (instantiate $m (with "h" (instance $h)))))"#,
    )
    .unwrap();
    let err = glue_error(&component);
    assert!(err.contains("a:b/i#f") && err.contains("utf8"), "{err}");
}

#[test]
fn an_import_lowered_as_utf16_without_a_string_is_still_served() {
    // The encoding option alone is no reason to refuse: nothing here is a string.
    let component = lowering("u32", "i32", "string-encoding=utf16");
    let glue = wasm2rs::wit_host_glue(&component);
    assert!(glue.is_ok(), "{glue:?}");
}
