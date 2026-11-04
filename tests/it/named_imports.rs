//! With `TranspileOptions::named_imports`, the `Imports` trait methods are named
//! after the import (`env_log`) instead of its index (`import0`).

use std::process::Command;

use crate::common;
use wasm2rs::{TranspileOptions, transpile, transpile_with_options};

fn named() -> TranspileOptions {
    TranspileOptions {
        named_imports: true,
        ..TranspileOptions::default()
    }
}

/// Transpile `wat` with `named_imports`, append `main_body`, run it, assert exit 0.
fn run_named(name: &str, wat: &str, main_body: &str) {
    let wasm = wat::parse_str(wat).unwrap();
    let generated = transpile_with_options(&wasm, &named()).unwrap();
    let program = format!("{generated}\nfn main() {{\n{main_body}\n}}\n");
    let bin = common::build(name, &program);
    let status = Command::new(&bin).status().unwrap();
    assert!(status.success(), "program assertions failed: {name}");
}

#[test]
fn imports_are_called_through_their_names() {
    run_named(
        "named_imports_basic",
        r#"(module
          (import "env" "log" (func (param i32)))
          (import "env" "mul-2" (func (param i32) (result i32)))
          (func (param i32) (result i32)
            (call 0 (local.get 0))
            (call 1 (local.get 0))))"#,
        "struct Host { logged: i32 }\n\
         impl Imports for Host {\n\
             fn env_log(&mut self, a0: i32) { self.logged = a0; }\n\
             fn env_mul_2(&mut self, a0: i32) -> i32 { a0 * 2 }\n\
         }\n\
         let mut inst = Instance::new(Host { logged: 0 });\n\
         assert_eq!(inst.func2(5), 10);",
    );
}

#[test]
fn a_wasi_style_import_name_drops_the_version_and_punctuation() {
    run_named(
        "named_imports_wasi_style",
        r#"(module
          (import "wasi:io/streams@0.2.0" "[method]output-stream.write" (func (param i32) (result i32)))
          (func (param i32) (result i32) (call 0 (local.get 0))))"#,
        "struct Host;\n\
         impl Imports for Host {\n\
             fn wasi_io_streams_method_output_stream_write(&mut self, a0: i32) -> i32 { a0 + 1 }\n\
         }\n\
         let mut inst = Instance::new(Host);\n\
         assert_eq!(inst.func1(1), 2);",
    );
}

#[test]
fn clashing_and_duplicate_names_stay_unique() {
    // `memory` clashes with a trait member, `get_global0` with a generated
    // accessor, `a.b`/`a_b` collapse to the same name, `type` is a keyword.
    run_named(
        "named_imports_clash",
        r#"(module
          (import "" "memory" (func (result i32)))
          (import "x" "get_global0" (func (result i32)))
          (import "a" "b" (func (result i32)))
          (import "a_b" "" (func (result i32)))
          (import "" "type" (func (result i32)))
          (func (result i32)
            (i32.add (i32.add (call 0) (call 1))
              (i32.add (i32.add (call 2) (call 3)) (call 4)))))"#,
        "struct Host;\n\
         impl Imports for Host {\n\
             fn memory_(&mut self) -> i32 { 1 }\n\
             fn x_get_global0(&mut self) -> i32 { 10 }\n\
             fn a_b(&mut self) -> i32 { 100 }\n\
             fn a_b_3(&mut self) -> i32 { 1000 }\n\
             fn type_(&mut self) -> i32 { 10000 }\n\
         }\n\
         let mut inst = Instance::new(Host);\n\
         assert_eq!(inst.func5(), 11111);",
    );
}

#[test]
fn default_output_is_unchanged() {
    let wasm = wat::parse_str(
        r#"(module (import "env" "f" (func (result i32))) (func (result i32) call 0))"#,
    )
    .unwrap();
    let default = transpile(&wasm).unwrap();
    assert!(default.contains("fn import0("), "{default}");
    let off = transpile_with_options(&wasm, &TranspileOptions::default()).unwrap();
    assert_eq!(default, off);
    let on = transpile_with_options(&wasm, &named()).unwrap();
    assert!(on.contains("fn env_f("), "{on}");
    assert!(!on.contains("fn import0("), "{on}");
}
