//! `TranspileOptions::entry_exports`: only the functions reachable from the
//! chosen exports (plus the start function and the entry points the generated
//! code itself calls) are emitted. Function names keep their wasm index.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]

use std::process::Command;

use crate::common;

use wasm2rs::{TranspileOptions, transpile, transpile_with_options};

fn opts(entries: &[&str]) -> TranspileOptions {
    TranspileOptions {
        entry_exports: Some(entries.iter().map(|s| (*s).to_string()).collect()),
        ..TranspileOptions::default()
    }
}

fn generate(wat: &str, entries: &[&str]) -> String {
    let wasm = wat::parse_str(wat).expect("valid wat");
    transpile_with_options(&wasm, &opts(entries)).expect("transpile ok")
}

/// Generate with `entries`, append `main_body`, compile and run it.
fn run(name: &str, wat: &str, entries: &[&str], main_body: &str) {
    let generated = generate(wat, entries);
    let program = format!("{generated}\nfn main() {{\n{main_body}\n}}\n");
    let bin = common::build(name, &program);
    let status = Command::new(&bin).status().expect("run generated binary");
    assert!(
        status.success(),
        "generated program assertions failed: {name}"
    );
}

/// `a` (func 0) calls the unexported helper (func 3); `b` and `c` are unrelated.
const DIRECT: &str = r#"(module
    (func (export "a") (result i32) call 3)
    (func (export "b") (result i32) i32.const 2)
    (func (export "c") (result i32) i32.const 3)
    (func (result i32) i32.const 42))"#;

#[test]
fn only_the_chosen_export_and_its_callees_are_emitted() {
    let code = generate(DIRECT, &["a"]);
    assert!(code.contains("fn func0("), "{code}");
    assert!(code.contains("fn func3("), "{code}");
    assert!(!code.contains("fn func1("), "{code}");
    assert!(!code.contains("fn func2("), "{code}");
}

#[test]
fn the_reduced_module_still_compiles_and_runs() {
    run("entry_direct", DIRECT, &["a"], "assert_eq!(func0(), 42);");
}

#[test]
fn an_empty_root_list_drops_every_function() {
    let code = generate(DIRECT, &[]);
    assert!(!code.contains("fn func"), "{code}");
}

#[test]
fn without_the_option_the_output_is_unchanged() {
    let wasm = wat::parse_str(DIRECT).unwrap();
    assert_eq!(
        transpile(&wasm).unwrap(),
        transpile_with_options(&wasm, &TranspileOptions::default()).unwrap()
    );
}

#[test]
fn an_unknown_entry_export_is_an_error() {
    let wasm = wat::parse_str(DIRECT).unwrap();
    let err = transpile_with_options(&wasm, &opts(&["nope"])).unwrap_err();
    assert!(err.to_string().contains("nope"), "{err}");
}

/// `run` calls through the table; `slot` is only reachable from the table.
const INDIRECT: &str = r#"(module
    (type $t (func (result i32)))
    (table 1 funcref)
    (elem (i32.const 0) $slot)
    (func (export "run") (result i32) i32.const 0 call_indirect (type $t))
    (func $slot (type $t) i32.const 7)
    (func (export "other") (result i32) i32.const 9))"#;

#[test]
fn a_reachable_call_indirect_keeps_the_table_entries() {
    let code = generate(INDIRECT, &["run"]);
    assert!(code.contains("fn func1("), "{code}");
    assert!(!code.contains("fn func2("), "{code}");
    run(
        "entry_indirect",
        INDIRECT,
        &["run"],
        "let mut inst = Instance::new(); assert_eq!(inst.func0(), 7);",
    );
}

#[test]
fn table_entries_are_dropped_when_no_reachable_code_reads_the_table() {
    // `other` never touches the table, so `slot` (func 1) is dead too.
    let code = generate(INDIRECT, &["other"]);
    assert!(code.contains("fn func2("), "{code}");
    assert!(!code.contains("fn func1("), "{code}");
    assert!(!code.contains("fn func0("), "{code}");
    run(
        "entry_table_dropped",
        INDIRECT,
        &["other"],
        "let mut inst = Instance::new(); assert_eq!(inst.func2(), 9);",
    );
}

#[test]
fn ref_func_keeps_its_target() {
    let wat = r#"(module
        (type $t (func (result i32)))
        (table 1 funcref)
        (elem declare func $target)
        (func $target (type $t) i32.const 5)
        (func (export "a") (result i32)
          i32.const 0 ref.func $target table.set 0
          i32.const 0 call_indirect (type $t))
        (func (export "b") (result i32) i32.const 1))"#;
    let code = generate(wat, &["a"]);
    assert!(code.contains("fn func0("), "{code}");
    assert!(code.contains("fn func1("), "{code}");
    assert!(!code.contains("fn func2("), "{code}");
}

#[test]
fn the_start_function_is_always_a_root() {
    let wat = r#"(module
        (memory 1)
        (func $init i32.const 0 i32.const 5 i32.store)
        (start $init)
        (func (export "get") (result i32) i32.const 0 i32.load)
        (func (export "unused") (result i32) i32.const 1))"#;
    let code = generate(wat, &["get"]);
    assert!(code.contains("fn func0("), "{code}");
    assert!(!code.contains("fn func2("), "{code}");
    run(
        "entry_start",
        wat,
        &["get"],
        "let mut inst = Instance::new(); assert_eq!(inst.func1(), 5);",
    );
}

#[test]
fn wasi_thread_start_is_kept_as_an_internal_entry_point() {
    let wat = r#"(module
        (import "env" "memory" (memory 1 1 shared))
        (import "wasi" "thread-spawn" (func $spawn (param i32) (result i32)))
        (func (export "wasi_thread_start") (param i32 i32))
        (func (export "run") (result i32) i32.const 0 call $spawn))"#;
    let code = generate(wat, &[]);
    assert!(code.contains("fn func1("), "{code}");
    assert!(!code.contains("fn func2("), "{code}");
    let program = format!("{code}\nfn main() {{}}\n");
    common::build("entry_thread_start", &program);
}

#[test]
fn the_split_output_skips_dead_functions_too() {
    let wasm = wat::parse_str(DIRECT).unwrap();
    let mut files = Vec::new();
    wasm2rs::transpile_split_with_options(
        &wasm,
        &wasm2rs::SplitOptions {
            funcs_per_file: 1,
            max_bytes_per_file: 0,
        },
        &opts(&["a"]),
        |f| {
            files.push(f);
            Ok(())
        },
    )
    .unwrap();
    let all: String = files.iter().map(|f| f.code.as_str()).collect();
    assert!(
        all.contains("fn func0(") && all.contains("fn func3("),
        "{all}"
    );
    assert!(
        !all.contains("fn func1(") && !all.contains("fn func2("),
        "{all}"
    );
    // Two live functions at one per file, plus the root.
    assert_eq!(
        files.len(),
        3,
        "{:?}",
        files.iter().map(|f| &f.name).collect::<Vec<_>>()
    );
}

#[test]
fn the_cli_flag_selects_the_roots() {
    let wasm = wat::parse_str(DIRECT).unwrap();
    let dir = std::env::temp_dir().join("wasm2rs_entry_exports_cli");
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("in.wasm");
    std::fs::write(&input, wasm).unwrap();

    let run = |flag: &str| {
        let out = Command::new(env!("CARGO_BIN_EXE_wasm2rs"))
            .arg(flag)
            .arg(&input)
            .output()
            .unwrap();
        (out.status.success(), String::from_utf8(out.stdout).unwrap())
    };
    let (ok, code) = run("--entry-exports=a");
    assert!(ok);
    assert!(
        code.contains("fn func3(") && !code.contains("fn func1("),
        "{code}"
    );

    // An empty list keeps no export as a root; an unknown name fails.
    let (ok, code) = run("--entry-exports=");
    assert!(ok);
    assert!(!code.contains("fn func"), "{code}");
    let (ok, _) = run("--entry-exports=nope");
    assert!(!ok);
}
