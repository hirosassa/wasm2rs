//! `reentrant_imports`: every `Imports` method also receives the `Instance`, so a
//! host can call back into guest functions and read or write guest memory while an
//! import runs (what a WASI preview2 host needs for `cabi_realloc`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::arithmetic_side_effects,
    clippy::float_cmp,
    clippy::lossy_float_literal,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::unwrap_in_result,
    reason = "test code"
)]

use std::process::Command;

fn options() -> wasm2rs::TranspileOptions {
    wasm2rs::TranspileOptions {
        reentrant_imports: true,
        ..wasm2rs::TranspileOptions::default()
    }
}

fn compile(test: &str, wat: &str, extra: &str) -> std::path::PathBuf {
    let wasm = wat::parse_str(wat).expect("valid wat");
    let generated = wasm2rs::transpile_with_options(&wasm, &options()).expect("transpile ok");

    let dir = std::env::temp_dir().join(format!("wasm2rs_reentrant_{test}"));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let src = dir.join("gen.rs");
    let bin = dir.join(if cfg!(windows) { "gen.exe" } else { "gen" });
    std::fs::write(&src, format!("{generated}\n{extra}\n")).expect("write source");

    let out = Command::new("rustc")
        .current_dir(&dir)
        .arg(&src)
        .arg("--edition")
        .arg("2024")
        .arg("-D")
        .arg("warnings")
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("run rustc");
    assert!(
        out.status.success(),
        "generated code failed to compile:\n--- stderr ---\n{}",
        String::from_utf8_lossy(&out.stderr),
    );
    bin
}

// Function indices: 0 is the import, 1 is `run`, 2 is `double`, 3 is `again`.
const MODULE: &str = r#"
    (module
      (import "env" "ask" (func $ask (param i32) (result i32)))
      (memory (export "memory") 1)
      (func (export "run") (param i32) (result i32)
        (i32.store8 (i32.const 16) (i32.const 7))
        (call $ask (local.get 0)))
      (func $double (export "double") (param i32) (result i32)
        (i32.mul (local.get 0) (i32.const 2)))
      (func (export "again") (param i32) (result i32)
        (call $ask (local.get 0))))
    "#;

#[test]
fn a_host_calls_a_guest_function_and_reads_guest_memory_during_an_import() {
    let extra = r#"
        struct Host;
        impl Imports for Host {
            fn import0(&mut self, g: &mut Instance<Self>, x: i32) -> i32 {
                // The guest stored 7 before calling us; call back into it.
                assert_eq!(g.memory()[16], 7);
                g.memory()[17] = 9;
                g.func2(x) + 1
            }
        }
        fn main() {
            let mut inst = Instance::new(Host);
            assert_eq!(inst.func1(20), 41);
            assert_eq!(inst.memory()[17], 9);
            // The host is back in place for the next import call.
            assert_eq!(inst.func1(1), 3);
        }
    "#;
    let bin = compile("callback", MODULE, extra);
    let run = Command::new(&bin).output().expect("run generated binary");
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn an_import_reached_again_from_inside_an_import_traps() {
    // While the host runs it is lent out, so a guest function that calls an import
    // again has no host to call.
    let extra = r#"
        struct Host;
        impl Imports for Host {
            fn import0(&mut self, g: &mut Instance<Self>, x: i32) -> i32 {
                g.func3(x)
            }
        }
        fn main() {
            let mut inst = Instance::new(Host);
            inst.func3(1);
        }
    "#;
    let bin = compile("nested", MODULE, extra);
    let run = Command::new(&bin).output().expect("run generated binary");
    assert!(!run.status.success(), "a nested import call should trap");
    assert!(
        String::from_utf8_lossy(&run.stderr).contains("re-entrant"),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn arguments_read_from_memory_and_a_start_function_reach_the_host() {
    // The arguments are loads (they borrow the instance) and the start function
    // itself calls the import, before the instance is returned.
    let wat = r#"
        (module
          (import "env" "add" (func $add (param i32 i32) (result i32)))
          (memory (export "memory") 1)
          (data (i32.const 0) "\05\06")
          (func $boot (i32.store8 (i32.const 8)
            (call $add (i32.load8_u (i32.const 0)) (i32.load8_u (i32.const 1)))))
          (start $boot)
          (func (export "get") (result i32) (i32.load8_u (i32.const 8))))
        "#;
    let extra = r#"
        struct Host;
        impl Imports for Host {
            fn import0(&mut self, _g: &mut Instance<Self>, a: i32, b: i32) -> i32 { a + b }
        }
        fn main() {
            let mut inst = Instance::new(Host);
            assert_eq!(inst.func2(), 11);
        }
    "#;
    let bin = compile("start_and_args", wat, extra);
    let run = Command::new(&bin).output().expect("run generated binary");
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn reentrant_imports_is_refused_where_the_host_is_read_directly() {
    for (what, wat) in [
        (
            "imported memory",
            r#"(module (import "env" "f" (func)) (import "env" "m" (memory 1)))"#,
        ),
        (
            "imported global",
            r#"(module (import "env" "f" (func)) (import "env" "g" (global i32)))"#,
        ),
        (
            "imported table",
            r#"(module (import "env" "f" (func)) (import "env" "t" (table 1 funcref)))"#,
        ),
    ] {
        let wasm = wat::parse_str(wat).expect("valid wat");
        let err = wasm2rs::transpile_with_options(&wasm, &options())
            .err()
            .unwrap_or_else(|| panic!("{what} should be refused"));
        assert!(
            err.to_string().contains("reentrant_imports"),
            "{what}: {err}"
        );
    }
}

#[test]
fn reentrant_imports_changes_nothing_without_a_host_function_import() {
    let wat = "(module (memory 1) (func (result i32) (i32.const 7)))";
    let wasm = wat::parse_str(wat).expect("valid wat");
    let plain = wasm2rs::transpile(&wasm).expect("plain");
    let reentrant = wasm2rs::transpile_with_options(&wasm, &options()).expect("reentrant");
    assert_eq!(plain, reentrant);
}
