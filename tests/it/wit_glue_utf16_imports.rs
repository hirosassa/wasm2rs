//! A component whose imports are lowered with `string-encoding=utf16` (a MoonBit guest, #351): the
//! glue transcodes the strings a guest passes to a host import, and the ones the host returns,
//! between UTF-16 guest memory and a Rust `String`. Each test encodes a component with
//! `wit-component` (so the `canon lower` options are the real ones) around a hand-written core
//! module, appends the glue and an `impl Host`, and runs the result.

use std::process::Command;

use crate::common;
use wasm2rs::{TranspileOptions, WitOptions, transpile_with_options, wit_host_glue_with_options};
use wit_component::{ComponentEncoder, StringEncoding, embed_component_metadata};
use wit_parser::Resolve;

const WIT: &str = "package t:p;
    interface host { say: func(s: string); greet: func() -> string; }
    world w {
        import host;
        export send: func();
        export len-of-greeting: func() -> u32;
        export unit-of-greeting: func(i: u32) -> u32;
    }";

/// Linear memory plus a bump `cabi_realloc`, and the data the guest sends: `hé😀` (4 code units,
/// one surrogate pair) at 64, a lone high surrogate at 80.
fn core(send_body: &str) -> String {
    format!(
        r#"(module
      (import "t:p/host" "say" (func $say (param i32 i32)))
      (import "t:p/host" "greet" (func $greet (param i32)))
      (memory (export "memory") 1)
      (data (i32.const 64) "\68\00\e9\00\3d\d8\00\de")
      (data (i32.const 80) "\00\d8")
      (global $bump (mut i32) (i32.const 1024))
      (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32)
        (local $p i32)
        (local.set $p (i32.and (i32.add (global.get $bump) (i32.sub (local.get 2) (i32.const 1)))
                               (i32.sub (i32.const 0) (local.get 2))))
        (global.set $bump (i32.add (local.get $p) (local.get 3)))
        (local.get $p))
      (func (export "send") {send_body})
      (func (export "len-of-greeting") (result i32)
        (call $greet (i32.const 8)) (i32.load (i32.const 12)))
      (func (export "unit-of-greeting") (param i32) (result i32)
        (call $greet (i32.const 8))
        (i32.load16_u (i32.add (i32.load (i32.const 8)) (i32.shl (local.get 0) (i32.const 1))))))"#
    )
}

fn encoded(core: &str) -> Vec<u8> {
    let mut resolve = Resolve::default();
    let package = resolve.push_str("test.wit", WIT).unwrap();
    let world = resolve.select_world(&[package], Some("w")).unwrap();
    let mut module = wat::parse_str(core).unwrap();
    embed_component_metadata(&mut module, &resolve, world, StringEncoding::UTF16, false).unwrap();
    ComponentEncoder::default()
        .module(&module)
        .unwrap()
        .validate(true)
        .encode()
        .unwrap()
}

fn build_and_run(name: &str, send_body: &str, host: &str, main: &str) -> std::process::Output {
    let core = core(send_body);
    let component = encoded(&core);
    let options = WitOptions {
        exports: true,
        ..WitOptions::default()
    };
    let glue = wit_host_glue_with_options(&component, &options).unwrap();
    let opts = TranspileOptions::default()
        .with_named_imports(true)
        .with_reentrant_imports(true)
        .with_named_exports(true);
    let generated = transpile_with_options(&wat::parse_str(&core).unwrap(), &opts).unwrap();
    let program = format!("{generated}\n{glue}\n{host}\nfn main() {{ {main} }}\n");
    let bin = common::build(&format!("wit_glue_utf16_imports_{name}"), &program);
    Command::new(bin).output().unwrap()
}

/// A host that records what the guest says and greets with `greeting`.
fn host(greeting: &str) -> String {
    format!(
        "struct H {{ said: std::rc::Rc<std::cell::RefCell<Vec<String>>> }}\n\
         impl Host for H {{\n\
             fn t_p_host_say(&mut self, s: String) {{ self.said.borrow_mut().push(s); }}\n\
             fn t_p_host_greet(&mut self) -> String {{ {greeting:?}.to_string() }}\n\
         }}"
    )
}

fn assert_ok(out: &std::process::Output) {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_utf16_string_the_guest_passes_to_an_import_is_transcoded_with_surrogate_pairs() {
    let out = build_and_run(
        "say_pairs",
        "(call $say (i32.const 64) (i32.const 4))",
        &host(""),
        r#"let said = std::rc::Rc::new(std::cell::RefCell::new(vec![]));
           let mut inst = Instance::new(Glue(H { said: said.clone() }));
           Exports::send(&mut inst);
           assert_eq!(*said.borrow(), vec!["hé😀".to_string()]);"#,
    );
    assert_ok(&out);
}

#[test]
fn an_empty_utf16_string_passes_to_an_import() {
    let out = build_and_run(
        "say_empty",
        "(call $say (i32.const 64) (i32.const 0))",
        &host(""),
        r#"let said = std::rc::Rc::new(std::cell::RefCell::new(vec![]));
           let mut inst = Instance::new(Glue(H { said: said.clone() }));
           Exports::send(&mut inst);
           assert_eq!(*said.borrow(), vec![String::new()]);"#,
    );
    assert_ok(&out);
}

#[test]
fn an_unpaired_surrogate_passed_to_an_import_traps_instead_of_being_replaced() {
    let out = build_and_run(
        "say_lone",
        "(call $say (i32.const 80) (i32.const 1))",
        &host(""),
        r#"let said = std::rc::Rc::new(std::cell::RefCell::new(vec![]));
           let mut inst = Instance::new(Glue(H { said: said.clone() }));
           Exports::send(&mut inst);"#,
    );
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("invalid UTF-16"), "{stderr}");
}

#[test]
fn a_utf16_string_passed_to_an_import_at_an_odd_address_traps() {
    let out = build_and_run(
        "say_odd",
        "(call $say (i32.const 65) (i32.const 1))",
        &host(""),
        r#"let said = std::rc::Rc::new(std::cell::RefCell::new(vec![]));
           let mut inst = Instance::new(Glue(H { said: said.clone() }));
           Exports::send(&mut inst);"#,
    );
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("unaligned pointer"), "{stderr}");
}

#[test]
fn a_string_the_host_returns_is_written_to_the_guest_as_utf16() {
    // `😀` is one char and two code units: the length the guest reads counts units.
    let out = build_and_run(
        "greet",
        "nop",
        &host("hé😀"),
        r#"let said = std::rc::Rc::new(std::cell::RefCell::new(vec![]));
           let mut inst = Instance::new(Glue(H { said: said.clone() }));
           assert_eq!(Exports::len_of_greeting(&mut inst), 4);
           let units: Vec<u32> = (0..4).map(|i| Exports::unit_of_greeting(&mut inst, i)).collect();
           assert_eq!(units, vec![0x68, 0xE9, 0xD83D, 0xDE00]);"#,
    );
    assert_ok(&out);
}

#[test]
fn an_empty_string_the_host_returns_has_no_code_units() {
    let out = build_and_run(
        "greet_empty",
        "nop",
        &host(""),
        r#"let said = std::rc::Rc::new(std::cell::RefCell::new(vec![]));
           let mut inst = Instance::new(Glue(H { said: said.clone() }));
           assert_eq!(Exports::len_of_greeting(&mut inst), 0);"#,
    );
    assert_ok(&out);
}
