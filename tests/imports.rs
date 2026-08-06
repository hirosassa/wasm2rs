//! Integration tests for imported functions and globals via trait injection. A
//! module with imports generates a `pub trait Imports` (an `import{j}` method
//! per imported function, plus `get_global{k}`/`set_global{k}` per imported
//! global) and a generic `Instance<H: Imports>` that stores the host
//! implementation. Each test defines a host type, compiles the generated Rust
//! with `rustc -D warnings`, and drives the instance.

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

fn transpile_compile_run(test: &str, wat: &str, main_body: &str) {
    let wasm = wat::parse_str(wat).expect("valid wat");
    let generated = wasm2rs::transpile(&wasm).expect("transpile ok");

    let dir = std::env::temp_dir().join(format!("wasm2rs_imports_{test}"));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let src = dir.join("gen.rs");
    let bin = dir.join(if cfg!(windows) { "gen.exe" } else { "gen" });

    let program = format!("{generated}\nfn main() {{\n{main_body}\n}}\n");
    std::fs::write(&src, &program).expect("write generated source");

    let compile = Command::new("rustc")
        // Isolate each parallel rustc's codegen-unit temp objects per test dir.
        .current_dir(&dir)
        .arg(&src)
        .arg("--edition")
        .arg("2021")
        .arg("-D")
        .arg("warnings")
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("run rustc");
    assert!(
        compile.status.success(),
        "generated code failed to compile:\n{program}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&compile.stderr),
    );

    let run = Command::new(&bin).status().expect("run generated binary");
    assert!(
        run.success(),
        "generated program assertions failed:\n{program}"
    );
}

#[test]
fn direct_call_to_imported_function() {
    // Import occupies function index 0, so the defined function is index 1.
    transpile_compile_run(
        "direct",
        r#"
        (module
          (import "env" "add100" (func (param i32) (result i32)))
          (func (param i32) (result i32) (call 0 (local.get 0))))
        "#,
        "struct Host;\n    \
         impl Imports for Host {\n        \
             fn import0(&mut self, a0: i32) -> i32 { a0 + 100 }\n    \
         }\n    \
         let mut inst = Instance::new(Host);\n    \
         assert_eq!(inst.func1(5), 105);",
    );
}

#[test]
fn void_import_and_multiple_imports() {
    // Two imports (index 0 void, index 1 returning), defined function index 2.
    transpile_compile_run(
        "multi",
        r#"
        (module
          (import "env" "log" (func (param i32)))
          (import "env" "mul2" (func (param i32) (result i32)))
          (func (param i32) (result i32)
            (call 0 (local.get 0))
            (call 1 (local.get 0))))
        "#,
        "struct Host { logged: i32 }\n    \
         impl Imports for Host {\n        \
             fn import0(&mut self, a0: i32) { self.logged = a0; }\n        \
             fn import1(&mut self, a0: i32) -> i32 { a0 * 2 }\n    \
         }\n    \
         let mut inst = Instance::new(Host { logged: 0 });\n    \
         assert_eq!(inst.func2(5), 10);",
    );
}

#[test]
fn imported_immutable_global_is_read_from_host() {
    transpile_compile_run(
        "global_ro",
        r#"
        (module
          (import "env" "base" (global i32))
          (func (result i32) (global.get 0)))
        "#,
        "struct Host;\n    \
         impl Imports for Host {\n        \
             fn get_global0(&self) -> i32 { 42 }\n    \
         }\n    \
         let mut inst = Instance::new(Host);\n    \
         assert_eq!(inst.func0(), 42);",
    );
}

#[test]
fn imported_mutable_global_get_set_with_defined_global() {
    // Global 0 is the imported mutable global (host-backed); global 1 is a
    // defined mutable global, so its field must be named `g1`, not `g0`.
    transpile_compile_run(
        "global_rw",
        r#"
        (module
          (import "env" "counter" (global (mut i32)))
          (global (mut i32) (i32.const 5))
          (func (result i32)
            (global.set 0 (i32.add (global.get 0) (i32.const 1)))
            (global.set 1 (i32.add (global.get 1) (global.get 0)))
            (global.get 1)))
        "#,
        "struct Host { c: i32 }\n    \
         impl Imports for Host {\n        \
             fn get_global0(&self) -> i32 { self.c }\n        \
             fn set_global0(&mut self, v: i32) { self.c = v; }\n    \
         }\n    \
         let mut inst = Instance::new(Host { c: 10 });\n    \
         assert_eq!(inst.func0(), 16);\n    \
         assert_eq!(inst.func0(), 28);",
    );
}

#[test]
fn imported_function_and_global_share_no_index_space() {
    // Function import 0 and global import 0 are indexed independently.
    transpile_compile_run(
        "func_and_global",
        r#"
        (module
          (import "env" "f" (func (param i32) (result i32)))
          (import "env" "g" (global i32))
          (func (result i32) (call 0 (global.get 0))))
        "#,
        "struct Host;\n    \
         impl Imports for Host {\n        \
             fn import0(&mut self, a0: i32) -> i32 { a0 * 10 }\n        \
             fn get_global0(&self) -> i32 { 7 }\n    \
         }\n    \
         let mut inst = Instance::new(Host);\n    \
         assert_eq!(inst.func1(), 70);",
    );
}

#[test]
fn call_indirect_dispatches_to_imported_function() {
    // The table slot holds function index 0, which is the import, so the
    // indirect call must dispatch into the injected host implementation.
    transpile_compile_run(
        "indirect",
        r#"
        (module
          (type $unary (func (param i32) (result i32)))
          (import "env" "inc" (func (param i32) (result i32)))
          (table 1 funcref)
          (elem (i32.const 0) 0)
          (func (param i32) (result i32)
            (call_indirect (type $unary) (local.get 0) (i32.const 0))))
        "#,
        "struct Host;\n    \
         impl Imports for Host {\n        \
             fn import0(&mut self, a0: i32) -> i32 { a0 + 1 }\n    \
         }\n    \
         let mut inst = Instance::new(Host);\n    \
         assert_eq!(inst.func1(7), 8);",
    );
}
