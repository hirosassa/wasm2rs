//! `WitOptions::typed_traps`: the canonical-ABI traps of the glue unwind with a typed
//! `Wasm2RsGlueTrap` payload (`resume_unwind`) instead of a string panic. Each case runs
//! the same guest with the option on (and with a typed core too, which defines its own
//! `Wasm2RsTrap`) and off, so the typed variant and the unchanged string message are both
//! checked.

use std::process::Command;

use crate::common;
use crate::wit_glue::component;
use wasm2rs::{TranspileOptions, WitOptions, transpile_with_options, wit_host_glue_with_options};

const MEM: &str = r#"
    (memory (export "memory") 1)
    (global $bump (mut i32) (i32.const 1024))
    (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32)
      (local $p i32)
      (local.set $p (i32.and (i32.add (global.get $bump) (i32.sub (local.get 2) (i32.const 1)))
                             (i32.sub (i32.const 0) (local.get 2))))
      (global.set $bump (i32.add (local.get $p) (local.get 3)))
      (local.get $p))
"#;

fn build(
    name: &str,
    body: &str,
    core_wat: &str,
    rest: &str,
    glue_typed: bool,
    core_typed: bool,
) -> std::process::Output {
    let glue = wit_host_glue_with_options(
        &component(body),
        &WitOptions {
            typed_traps: glue_typed,
            ..WitOptions::default()
        },
    )
    .unwrap();
    let opts = TranspileOptions::default()
        .with_named_imports(true)
        .with_reentrant_imports(true)
        .with_named_exports(true)
        .with_typed_traps(core_typed);
    let core = transpile_with_options(&wat::parse_str(core_wat).unwrap(), &opts).unwrap();
    let bin = common::build(
        &format!("wit_glue_typed_{name}_{glue_typed}_{core_typed}"),
        &format!("{core}\n{glue}\n{rest}\n"),
    );
    Command::new(bin).output().unwrap()
}

/// Runs `inst.run()` of the guest that traps in the glue: typed, the payload is `variant`
/// (a `Wasm2RsGlueTrap` variant) and its `Display` is `message`; untyped, the panic text
/// contains `message` and the glue defines no trap type.
fn traps(
    name: &str,
    body: &str,
    imports: &str,
    guest: &str,
    host: &str,
    variant: &str,
    message: &str,
) {
    let core_wat = format!("(module {imports} {MEM} {guest})");
    for core_typed in [false, true] {
        let on = build(
            name,
            body,
            &core_wat,
            &format!(
                "{host}
                fn main() {{
                    let p = std::panic::catch_unwind(|| {{ Instance::new(Glue(H)).run(); }})
                        .unwrap_err();
                    let t = p.downcast::<Wasm2RsGlueTrap>().expect(\"a Wasm2RsGlueTrap payload\");
                    assert_eq!(*t, Wasm2RsGlueTrap::{variant});
                    assert_eq!(t.to_string(), \"wasm2rs wit glue: {message}\");
                    let _: &dyn std::error::Error = &*t;
                }}"
            ),
            true,
            core_typed,
        );
        assert!(
            on.status.success(),
            "{name} (typed core {core_typed}): {}",
            String::from_utf8_lossy(&on.stderr)
        );
    }
    let off = build(
        name,
        body,
        &core_wat,
        &format!("{host} fn main() {{ Instance::new(Glue(H)).run(); }}"),
        false,
        false,
    );
    let stderr = String::from_utf8_lossy(&off.stderr);
    assert!(!off.status.success(), "{name} did not panic");
    assert!(stderr.contains(message), "{name}: {stderr}");
}

const SHOUT: &str = r#"(export "shout" (func (param "s" string) (result string)))"#;
const SHOUT_IMPORT: &str = r#"(import "a:b/c" "shout" (func $shout (param i32 i32 i32)))"#;
const SHOUT_HOST: &str = r#"
    struct H;
    impl Host for H { fn a_b_c_shout(&mut self, s: String) -> String { s } }
"#;

#[test]
fn string_traps_are_typed() {
    traps(
        "utf8",
        SHOUT,
        SHOUT_IMPORT,
        r#"(data (i32.const 16) "\ff\fe")
        (func (export "run") (call $shout (i32.const 16) (i32.const 2) (i32.const 64)))"#,
        SHOUT_HOST,
        "InvalidUtf8",
        "invalid UTF-8 in a string",
    );
    traps(
        "string_oob",
        SHOUT,
        SHOUT_IMPORT,
        r#"(func (export "run") (call $shout (i32.const 65530) (i32.const 100) (i32.const 64)))"#,
        SHOUT_HOST,
        "ComponentPointerOutOfBounds",
        "pointer out of bounds",
    );
}

#[test]
fn list_traps_are_typed() {
    let body = r#"(export "nums" (func (param "l" (list u32)) (result u32)))"#;
    let imports = r#"(import "a:b/c" "nums" (func $nums (param i32 i32) (result i32)))"#;
    let host = r#"
        struct H;
        impl Host for H { fn a_b_c_nums(&mut self, l: Vec<u32>) -> u32 { l.len() as u32 } }
    "#;
    traps(
        "list_unaligned",
        body,
        imports,
        r#"(func (export "run") (result i32) (call $nums (i32.const 34) (i32.const 2)))"#,
        host,
        "ComponentUnalignedPointer",
        "unaligned pointer",
    );
    traps(
        "list_too_large",
        body,
        imports,
        r#"(func (export "run") (result i32) (call $nums (i32.const 32) (i32.const 0x40000000)))"#,
        host,
        "LiftedDataTooLarge",
        "lifted data too large",
    );
}

#[test]
fn value_traps_are_typed() {
    traps(
        "bad_char",
        r#"(export "c" (func (param "c" char) (result u32)))"#,
        r#"(import "a:b/c" "c" (func $c (param i32) (result i32)))"#,
        r#"(func (export "run") (result i32) (call $c (i32.const 0xD800)))"#,
        "struct H; impl Host for H { fn a_b_c_c(&mut self, c: char) -> u32 { c as u32 } }",
        "InvalidChar",
        "invalid char",
    );
    traps(
        "bad_disc",
        r#"(export "o" (func (param "o" (option u8)) (result u32)))"#,
        r#"(import "a:b/c" "o" (func $o (param i32 i32) (result i32)))"#,
        r#"(func (export "run") (result i32) (call $o (i32.const 2) (i32.const 0)))"#,
        "struct H; impl Host for H { fn a_b_c_o(&mut self, o: Option<u8>) -> u32 { o.is_some() as u32 } }",
        "InvalidDiscriminant",
        "invalid discriminant",
    );
}

#[test]
fn a_return_pointer_out_of_bounds_is_typed() {
    traps(
        "retptr_oob",
        r#"(type $t (record (field "a" u32) (field "b" u8) (field "c" u64)))
        (export "triple" (type $tr (eq $t)))
        (export "mk" (func (param "n" u32) (result $tr)))"#,
        r#"(import "a:b/c" "mk" (func $mk (param i32 i32)))"#,
        r#"(func (export "run") (call $mk (i32.const 0) (i32.const 65528)))"#,
        "struct H; impl Host for H { fn a_b_c_mk(&mut self, n: u32) -> Triple { Triple { a: n, b: 7, c: 1 } } }",
        "ComponentPointerOutOfBounds",
        "pointer out of bounds",
    );
}

#[test]
#[cfg(target_pointer_width = "64")]
fn a_returned_list_over_the_guest_address_space_is_typed() {
    // 2^32 bytes: one past what a guest `(ptr, len)` can describe. The zeroed Vec is
    // allocated lazily, and the glue refuses it before it touches a byte.
    traps(
        "lowered_too_large",
        r#"(export "blob" (func (param "n" u32) (result (list u8))))"#,
        r#"(import "a:b/c" "blob" (func $blob (param i32 i32)))"#,
        r#"(func (export "run") (call $blob (i32.const 0) (i32.const 64)))"#,
        "struct H; impl Host for H { fn a_b_c_blob(&mut self, _n: u32) -> Vec<u8> { vec![0u8; 1usize << 32] } }",
        "LoweredDataTooLarge",
        "list too large",
    );
}

#[test]
fn the_option_is_on_by_default_and_changes_only_the_traps() {
    let wasm = component(SHOUT);
    let default = wit_host_glue_with_options(&wasm, &WitOptions::default()).unwrap();
    assert_eq!(default, wasm2rs::wit_host_glue(&wasm).unwrap());
    assert!(default.contains("pub enum Wasm2RsGlueTrap"), "{default}");
    assert!(
        default.contains("Wasm2RsGlueTrap::InvalidUtf8"),
        "{default}"
    );
    // Only the variants the glue can raise are defined: no async export, no callback code.
    assert!(!default.contains("InvalidCallbackCode"), "{default}");
    assert!(!default.contains("panic!"), "{default}");

    let untyped = wit_host_glue_with_options(
        &wasm,
        &WitOptions {
            typed_traps: false,
            ..WitOptions::default()
        },
    )
    .unwrap();
    assert!(!untyped.contains("Wasm2RsGlueTrap"), "{untyped}");
    assert!(
        untyped.contains("panic!(\"wasm2rs wit glue: {msg}\")"),
        "{untyped}"
    );
}

#[test]
fn async_scaffolding_and_unimplemented_stay_panics() {
    let wasm = component(SHOUT);
    let typed = wit_host_glue_with_options(
        &wasm,
        &WitOptions {
            typed_traps: true,
            default_trap: true,
            ..WitOptions::default()
        },
    )
    .unwrap();
    // The trap type is a glue concern; a default `Host` method is not one.
    assert!(typed.contains("Wasm2RsGlueTrap"), "{typed}");
    let trait_text = wasm2rs::wit_host_trait_with_options(
        &wasm,
        &WitOptions {
            default_trap: true,
            ..WitOptions::default()
        },
    )
    .unwrap();
    assert!(trait_text.contains("not implemented"), "{trait_text}");
    assert!(!trait_text.contains("Wasm2RsGlueTrap"), "{trait_text}");
}
