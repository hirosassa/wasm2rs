//! Edges of the component reader: which core module `transpile_component` picks, the module
//! index space it tracks, and the `canon lift` shapes `wit_exports` refuses.

use wasm2rs::{ComponentOptions, TranspileError, transpile_component, wit_exports};

fn component(text: &str) -> Vec<u8> {
    wat::parse_str(text).unwrap()
}

fn transpile(text: &str) -> Result<String, TranspileError> {
    transpile_component(&component(text), &ComponentOptions::default())
}

fn unsupported(text: &str) -> String {
    let err = transpile(text).unwrap_err();
    assert!(matches!(err, TranspileError::Unsupported(_)), "{err:?}");
    err.to_string()
}

fn rejection(text: &str) -> String {
    wit_exports(&component(text)).unwrap_err().to_string()
}

fn code_of(text: &str) -> String {
    transpile(text).unwrap()
}

/// Instantiates core module `${module}` and exports a lift of its core export `export` as
/// `e{module}`.
fn lift_of(module: &str, export: &str) -> String {
    format!(
        r#"(core instance $i{module} (instantiate ${module}))
  (alias core export $i{module} "{export}" (core func $f{module}))
  (func $l{module} (canon lift (core func $f{module})))
  (export "e{module}" (func $l{module}))"#
    )
}

// --- module selection ---

#[test]
fn exports_lifted_from_two_core_modules_are_refused_naming_both() {
    let why = unsupported(&format!(
        r#"(component
  (core module $a (func (export "fa")))
  (core module $b (func (export "fb")))
  {}
  {})"#,
        lift_of("a", "fa"),
        lift_of("b", "fb")
    ));
    assert!(
        why.contains("more than one core module") && why.contains("`fa`") && why.contains("`fb`"),
        "{why}"
    );
}

#[test]
fn several_drivable_modules_without_a_lowered_import_receiver_have_no_program() {
    // Nothing is exported and neither module is instantiated with lowered imports.
    let why = unsupported(
        r#"(component
  (core module $a (func (export "a")))
  (core module $b (func (export "b")))
  (core instance (instantiate $a))
  (core instance (instantiate $b)))"#,
    );
    assert!(why.contains("no program to pick"), "{why}");
}

const LOWERED_PREFIX: &str = r#"(import "t:t/i" (instance $i (export "f" (func))))
  (alias export $i "f" (func $f))
  (core func $l (canon lower (func $f)))
  (core instance $env (export "f" (func $l)))"#;

#[test]
fn a_receiver_that_exports_nothing_and_has_no_start_is_not_the_program() {
    // `$wire` receives the lowered import but cannot be driven.
    let why = unsupported(&format!(
        r#"(component
  {LOWERED_PREFIX}
  (core module $wire (import "env" "f" (func)))
  (core module $program (func (export "marker_program")))
  (core instance (instantiate $wire (with "env" (instance $env))))
  (core instance (instantiate $program)))"#
    ));
    assert!(why.contains("no program to pick"), "{why}");
}

#[test]
fn a_receiver_with_only_a_start_function_is_drivable() {
    let code = code_of(&format!(
        r#"(component
  {LOWERED_PREFIX}
  (core module $other (func (export "other")))
  (core module $program
    (import "env" "f" (func $f))
    (func $s call $f)
    (start $s))
  (core instance (instantiate $other))
  (core instance (instantiate $program (with "env" (instance $env)))))"#
    ));
    assert!(
        code.contains("fn env_f"),
        "the start-only receiver is the program"
    );
    assert!(!code.contains("fn other"), "{code}");
}

// --- the module index space ---

#[test]
fn an_outer_alias_of_a_module_repeats_its_entry() {
    // `$alias` (index 2) is the same module as `$program` (index 0).
    let text = format!(
        r#"(component
  {LOWERED_PREFIX}
  (core module $program (import "env" "f" (func)) (func (export "marker_program")))
  (core module $other (func (export "other")))
  (alias outer 0 $program (core module $alias))
  (core instance (instantiate $other))
  (core instance (instantiate $alias (with "env" (instance $env)))))"#
    );
    assert!(code_of(&text).contains("fn marker_program"));
}

#[test]
fn a_module_taken_from_an_imported_instance_is_never_a_candidate() {
    // The only receiver is the aliased module (not read), so there is no program.
    let why = unsupported(&format!(
        r#"(component
  {LOWERED_PREFIX}
  (import "x" (instance $x (export "m" (core module (import "env" "f" (func)) (export "g" (func))))))
  (alias export $x "m" (core module $im))
  (core module $a (func (export "a")))
  (core module $b (func (export "b")))
  (core instance (instantiate $a))
  (core instance (instantiate $im (with "env" (instance $env)))))"#
    ));
    assert!(why.contains("no program to pick"), "{why}");
}

// --- shapes `wit_exports` refuses ---

#[test]
fn a_function_import_is_refused_before_the_exports_are_read() {
    let err = rejection(
        r#"(component
  (import "f" (func $f))
  (export "g" (func $f)))"#,
    );
    // The reader refuses a function import before it looks at the exports.
    assert!(
        err.contains("\"f\"") && err.contains("not an interface instance"),
        "{err}"
    );
}

#[test]
fn an_exported_instance_that_is_an_import_is_refused() {
    let err = rejection(
        r#"(component
  (import "a:b/i" (instance $i (export "f" (func))))
  (export "a:b/i" (instance $i)))"#,
    );
    assert!(
        err.contains("`a:b/i`") && err.contains("not made of this component's own"),
        "{err}"
    );
}

#[test]
fn a_function_aliased_from_an_imported_instance_is_not_a_lift() {
    let err = rejection(
        r#"(component
  (import "a:b/i" (instance $i (export "f" (func))))
  (alias export $i "f" (func $f))
  (export "g" (func $f)))"#,
    );
    assert!(err.contains("`g`") && err.contains("alias"), "{err}");
}

#[test]
fn a_memory_from_another_core_instance_is_refused() {
    let err = rejection(
        r#"(component
  (core module $a (memory (export "mem") 1))
  (core module $b
    (func (export "alloc") (param i32 i32 i32 i32) (result i32) i32.const 0)
    (func (export "f") (param i32 i32)))
  (core instance $ia (instantiate $a))
  (core instance $ib (instantiate $b))
  (alias core export $ia "mem" (core memory $m))
  (alias core export $ib "alloc" (core func $r))
  (alias core export $ib "f" (core func $f))
  (type $t (func (param "s" string)))
  (func $l (type $t) (canon lift (core func $f) (memory $m) (realloc $r)))
  (export "say" (func $l)))"#,
    );
    assert!(
        err.contains("`say`") && err.contains("memory") && err.contains("same core instance"),
        "{err}"
    );
}

#[test]
fn outer_aliases_and_exports_of_components_and_modules_do_not_disturb_the_exports() {
    // Items the reader only counts: they must not shift the module ordinal or the funcs.
    let exports = wit_exports(&component(
        r#"(component
  (component $c)
  (export "c" (component $c))
  (alias outer 0 $c (component $c2))
  (core module $a (func (export "f")))
  (alias outer 0 $a (core module $a2))
  (core instance $i (instantiate $a2))
  (alias core export $i "f" (core func $f))
  (func $l (canon lift (core func $f)))
  (export "go" (func $l)))"#,
    ))
    .unwrap();
    assert_eq!(exports.len(), 1);
    assert_eq!(exports[0].core_export, "f");
    assert_eq!(exports[0].core_module, 0);
}
