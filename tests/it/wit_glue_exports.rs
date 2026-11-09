//! `WitOptions::exports`: the generated `Exports` trait calls a component's exported
//! functions with typed arguments and results. Each test encodes a component around a
//! hand-written core module with `wit-component` (so the `canon lift` options are the real
//! ones), transpiles the module with `named_imports` + `reentrant_imports` + `named_exports`,
//! appends the glue and calls the typed methods.
//!
//! The typed methods are called as `Exports::name(&mut inst, ..)`: the transpiled `Instance`
//! has an inherent method of the same name (the flat core export), which a method call
//! `inst.name(..)` would pick instead.

use std::process::Command;

use crate::common;
use wasm2rs::{TranspileOptions, WitOptions, transpile_with_options, wit_host_glue_with_options};
use wit_component::{ComponentEncoder, StringEncoding, embed_component_metadata};
use wit_parser::Resolve;

/// The one import every test module has: the transpiled `Instance` is generic over its
/// imports (`Imports`, implemented by `Glue`) only when the module imports something.
const HOST_WIT: &str = "package h:h; interface host { ping: func(); }";

/// `core` with the import of `h:h/host#ping` added.
fn with_host_import(core: &str) -> String {
    core.replacen("(module", r#"(module (import "h:h/host" "ping" (func))"#, 1)
}

/// A component of the world `w` in `wit` (which also imports `h:h/host`), around the core
/// module `core` (to which the matching import is added).
fn encoded(wit: &str, core: &str) -> Vec<u8> {
    let wit = wit.replacen("world w {", "world w { import h:h/host;", 1);
    let core = with_host_import(core);
    let mut resolve = Resolve::default();
    resolve.push_str("host.wit", HOST_WIT).unwrap();
    let package = resolve.push_str("test.wit", &wit).unwrap();
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

fn exports_on() -> WitOptions {
    WitOptions {
        exports: true,
        ..WitOptions::default()
    }
}

/// Builds the program (transpiled `core`, the glue with exports, then a `main` running
/// `body` on `inst`) and returns its output.
fn build_and_run(name: &str, wit: &str, core: &str, body: &str) -> std::process::Output {
    let component = encoded(wit, core);
    let glue = wit_host_glue_with_options(&component, &exports_on()).unwrap();
    let opts = TranspileOptions {
        named_imports: true,
        reentrant_imports: true,
        named_exports: true,
        ..TranspileOptions::default()
    };
    let core = wat::parse_str(with_host_import(core)).unwrap();
    let generated = transpile_with_options(&core, &opts).unwrap();
    let program = format!(
        "{generated}\n{glue}\nstruct H;\nimpl Host for H {{ fn h_h_host_ping(&mut self) {{}} }}\n\
         fn main() {{ let mut inst = Instance::new(Glue(H)); {body} }}\n"
    );
    let bin = common::build(&format!("wit_glue_exports_{name}"), &program);
    Command::new(bin).output().unwrap()
}

fn run(name: &str, wit: &str, core: &str, body: &str) {
    let out = build_and_run(name, wit, core, body);
    assert!(
        out.status.success(),
        "{name}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Linear memory plus a bump `cabi_realloc` (aligns the pointer, never frees).
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

const RUN_WIT: &str = "package wasi:cli@0.2.0;
interface run { run: func() -> result; }
world w { export run; }";

#[test]
fn a_cli_run_export_without_arguments_returns_the_typed_result() {
    let wit = RUN_WIT;
    let ok = r#"(module (func (export "wasi:cli/run@0.2.0#run") (result i32) i32.const 0))"#;
    let err = r#"(module (func (export "wasi:cli/run@0.2.0#run") (result i32) i32.const 1))"#;
    run(
        "run_ok",
        wit,
        ok,
        "assert_eq!(Exports::export_wasi_cli_run_0_2_0_run(&mut inst), Ok(()));",
    );
    run(
        "run_err",
        wit,
        err,
        "assert_eq!(Exports::export_wasi_cli_run_0_2_0_run(&mut inst), Err(()));",
    );
}

#[test]
fn the_trait_declares_one_method_per_export_named_like_the_core_export() {
    let wit = "package t:p;
interface i { f: func(a: u32) -> u32; g-h: func(); }
world w { export i; export top-level: func(); }";
    let core = r#"(module
      (func (export "t:p/i#f") (param i32) (result i32) local.get 0)
      (func (export "t:p/i#g-h"))
      (func (export "top-level")))"#;
    let glue = wit_host_glue_with_options(&encoded(wit, core), &exports_on()).unwrap();
    assert!(glue.contains("pub trait Exports {"), "{glue}");
    for sig in [
        "fn t_p_i_f(&mut self, a: u32) -> u32;",
        "fn t_p_i_g_h(&mut self);",
        "fn top_level(&mut self);",
    ] {
        assert!(glue.contains(sig), "missing `{sig}` in {glue}");
    }
    assert!(
        glue.contains("impl<H: Host> Exports for Instance<Glue<H>> {"),
        "{glue}"
    );
}

#[test]
fn exports_are_off_by_default() {
    let core = r#"(module (func (export "wasi:cli/run@0.2.0#run") (result i32) i32.const 0))"#;
    let component = encoded(RUN_WIT, core);
    let glue = wit_host_glue_with_options(&component, &WitOptions::default()).unwrap();
    assert!(!glue.contains("Exports"), "{glue}");
}

#[test]
fn flat_scalar_params_and_a_flat_result_are_lowered_and_lifted() {
    run(
        "scalars",
        "package t:p;
         world w { export calc: func(a: s32, b: f64, c: bool, d: u8) -> f64; }",
        r#"(module (func (export "calc") (param i32 f64 i32 i32) (result f64)
             (f64.add
               (select (f64.convert_i32_s (local.get 0)) (f64.const 0) (local.get 2))
               (f64.add (local.get 1) (f64.convert_i32_u (local.get 3))))))"#,
        "assert_eq!(Exports::calc(&mut inst, -3, 0.5, true, 255), 252.5);\n\
         assert_eq!(Exports::calc(&mut inst, -3, 0.5, false, 1), 1.5);",
    );
}

#[test]
fn a_string_param_and_a_string_result_go_through_guest_memory() {
    run(
        "string",
        "package t:p;
         world w { export echo: func(s: string) -> string; }",
        &format!(
            r#"(module {MEM}
              (func (export "echo") (param i32 i32) (result i32)
                (i32.store (i32.const 8) (local.get 0))
                (i32.store (i32.const 12) (i32.add (local.get 1) (i32.const 1)))
                (i32.store8 (i32.add (local.get 0) (local.get 1)) (i32.const 33))
                (i32.const 8)))"#
        ),
        r#"assert_eq!(Exports::echo(&mut inst, "héllo".to_string()), "héllo!");"#,
    );
}

#[test]
fn a_list_param_is_written_element_by_element() {
    run(
        "list",
        "package t:p;
         world w { export sum: func(l: list<u32>) -> u32; }",
        &format!(
            r#"(module {MEM}
              (func (export "sum") (param i32 i32) (result i32) (local $acc i32) (local $i i32)
                (loop $l
                  (if (i32.lt_u (local.get $i) (local.get 1))
                    (then
                      (local.set $acc (i32.add (local.get $acc)
                        (i32.load (i32.add (local.get 0) (i32.shl (local.get $i) (i32.const 2))))))
                      (local.set $i (i32.add (local.get $i) (i32.const 1)))
                      (br $l))))
                (local.get $acc)))"#
        ),
        "assert_eq!(Exports::sum(&mut inst, vec![1, 2, 39]), 42);\n\
         assert_eq!(Exports::sum(&mut inst, vec![]), 0);",
    );
}

#[test]
fn option_param_and_result_in_memory_are_lowered_and_lifted() {
    run(
        "option",
        "package t:p;
         world w { export inc: func(o: option<u32>) -> option<u32>; }",
        &format!(
            r#"(module {MEM}
              (func (export "inc") (param i32 i32) (result i32)
                (i32.store (i32.const 8) (local.get 0))
                (i32.store (i32.const 12) (i32.add (local.get 1) (i32.const 1)))
                (i32.const 8)))"#
        ),
        "assert_eq!(Exports::inc(&mut inst, Some(41)), Some(42));\n\
         assert_eq!(Exports::inc(&mut inst, None), None);",
    );
}

#[test]
fn a_result_with_a_payload_is_lifted_from_memory() {
    run(
        "result",
        "package t:p;
         world w { export parse: func(n: u32) -> result<u32, string>; }",
        &format!(
            r#"(module {MEM}
              (data (i32.const 64) "odd")
              (func (export "parse") (param i32) (result i32)
                (if (i32.and (local.get 0) (i32.const 1))
                  (then
                    (i32.store (i32.const 8) (i32.const 1))
                    (i32.store (i32.const 12) (i32.const 64))
                    (i32.store (i32.const 16) (i32.const 3)))
                  (else
                    (i32.store (i32.const 8) (i32.const 0))
                    (i32.store (i32.const 12) (i32.mul (local.get 0) (i32.const 10)))))
                (i32.const 8)))"#
        ),
        "assert_eq!(Exports::parse(&mut inst, 4), Ok(40));\n\
         assert_eq!(Exports::parse(&mut inst, 3), Err(\"odd\".to_string()));",
    );
}

#[test]
fn more_than_sixteen_flat_params_are_passed_as_a_record_in_guest_memory() {
    // 17 `u32` params: the core function takes one pointer to them.
    let params: Vec<String> = (0..17).map(|i| format!("p{i}: u32")).collect();
    let wit = format!(
        "package t:p; world w {{ export big: func({}) -> u32; }}",
        params.join(", ")
    );
    let args: Vec<String> = (1..=17).map(|i| i.to_string()).collect();
    run(
        "spilled",
        &wit,
        &format!(
            r#"(module {MEM}
              (func (export "big") (param i32) (result i32)
                (i32.add (i32.load (local.get 0)) (i32.load offset=64 (local.get 0)))))"#
        ),
        &format!(
            "assert_eq!(Exports::big(&mut inst, {}), 1 + 17);",
            args.join(", ")
        ),
    );
}

#[test]
fn post_return_is_called_with_the_core_result_after_the_result_is_lifted() {
    // The post-return stores its argument at 4 and counts calls at 0; the result string is
    // read first, so the guest may free it afterwards.
    run(
        "post_return",
        "package t:p; interface i { name: func() -> string; }
         world w { export i; }",
        &format!(
            r#"(module {MEM}
              (data (i32.const 64) "guest")
              (func (export "t:p/i#name") (result i32)
                (i32.store (i32.const 8) (i32.const 64))
                (i32.store (i32.const 12) (i32.const 5))
                (i32.const 8))
              (func (export "cabi_post_t:p/i#name") (param i32)
                (i32.store (i32.const 4) (local.get 0))
                (i32.store (i32.const 0) (i32.add (i32.load (i32.const 0)) (i32.const 1)))
                (i32.store (i32.const 64) (i32.const 0))))"#
        ),
        "assert_eq!(Exports::t_p_i_name(&mut inst), \"guest\");\n\
         let m = inst.memory();\n\
         assert_eq!(u32::from_le_bytes(m[0..4].try_into().unwrap()), 1);\n\
         assert_eq!(u32::from_le_bytes(m[4..8].try_into().unwrap()), 8);",
    );
}

#[test]
fn a_guest_trap_stays_a_panic() {
    let out = build_and_run(
        "trap",
        RUN_WIT,
        r#"(module (func (export "wasi:cli/run@0.2.0#run") (result i32) unreachable))"#,
        "let _ = Exports::export_wasi_cli_run_0_2_0_run(&mut inst);",
    );
    assert!(!out.status.success());
}

#[test]
fn an_invalid_returned_pointer_is_a_trap_not_a_bad_read() {
    let out = build_and_run(
        "bad_ptr",
        "package t:p; world w { export s: func() -> string; }",
        &format!(r#"(module {MEM} (func (export "s") (result i32) i32.const 0x7ffffff0))"#),
        "let _ = Exports::s(&mut inst);",
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(stderr.contains("pointer out of bounds"), "{stderr}");
}

#[test]
fn an_export_that_is_not_a_direct_lift_is_rejected_naming_it() {
    let component = wat::parse_str(
        r#"(component
  (core module $b (func (export "f")))
  (core instance $ib (instantiate $b))
  (alias core export $ib "f" (core func $f0))
  (core instance $shim (export "f" (func $f0)))
  (alias core export $shim "f" (core func $f))
  (type $t (func))
  (func $lf (type $t) (canon lift (core func $f)))
  (export "go" (func $lf)))"#,
    )
    .unwrap();
    let err = wit_host_glue_with_options(&component, &exports_on())
        .unwrap_err()
        .to_string();
    assert!(err.contains("`go`"), "{err}");
}

#[test]
fn exports_apply_to_the_glue_only() {
    let component = encoded(
        RUN_WIT,
        r#"(module (func (export "wasi:cli/run@0.2.0#run") (result i32) i32.const 0))"#,
    );
    let err = wasm2rs::wit_host_trait_with_options(&component, &exports_on())
        .unwrap_err()
        .to_string();
    assert!(err.contains("exports"), "{err}");
}
