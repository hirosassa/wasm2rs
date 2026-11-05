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
        .arg("2024")
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
fn a_global_initializer_may_read_an_imported_global() {
    // The initial value is the host's, read when the instance is built.
    transpile_compile_run(
        "global_init_imported",
        r#"
        (module
          (import "env" "base" (global i32))
          (global (mut i32) (global.get 0))
          (func (result i32) (global.get 1)))
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
fn an_extended_const_initializer_combines_an_imported_global() {
    transpile_compile_run(
        "global_init_imported_arith",
        r#"
        (module
          (import "env" "base" (global i64))
          (global $a i64 (i64.add (global.get 0) (i64.const 5)))
          (global i64 (i64.mul (global.get $a) (i64.const 2)))
          (func (result i64) (global.get 2)))
        "#,
        "struct Host;\n    \
         impl Imports for Host {\n        \
             fn get_global0(&self) -> i64 { 10 }\n    \
         }\n    \
         let mut inst = Instance::new(Host);\n    \
         assert_eq!(inst.func0(), 30);",
    );
}

#[test]
fn a_table_initializer_may_read_an_imported_funcref_global() {
    // Every initial entry holds the function the host's global names (function 0).
    transpile_compile_run(
        "table_init_imported",
        r#"
        (module
          (type $t (func (result i32)))
          (import "env" "f" (global funcref))
          (table 2 2 funcref (global.get 0))
          (func (type $t) (i32.const 7))
          (func (param i32) (result i32) (call_indirect (type $t) (local.get 0))))
        "#,
        "struct Host;\n    \
         impl Imports for Host {\n        \
             fn get_global0(&self) -> u32 { 0 }\n    \
         }\n    \
         let mut inst = Instance::new(Host);\n    \
         assert_eq!(inst.func1(0), 7);\n    \
         assert_eq!(inst.func1(1), 7);",
    );
}

#[test]
fn an_active_element_item_may_read_an_imported_funcref_global() {
    // Slot 0 stays null (calling it traps); slot 1 holds the host's function.
    transpile_compile_run(
        "elem_item_imported",
        r#"
        (module
          (type $t (func (result i32)))
          (import "env" "f" (global funcref))
          (table 2 funcref)
          (elem (i32.const 1) funcref (global.get 0))
          (func (type $t) (i32.const 7))
          (func (param i32) (result i32) (call_indirect (type $t) (local.get 0))))
        "#,
        "struct Host;\n    \
         impl Imports for Host {\n        \
             fn get_global0(&self) -> u32 { 0 }\n    \
         }\n    \
         let mut inst = Instance::new(Host);\n    \
         assert_eq!(inst.func1(1), 7);\n    \
         assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| inst.func1(0))).is_err());",
    );
}

#[test]
fn a_data_offset_may_read_an_imported_global() {
    // The segment lands where the host's global says, resolved at instantiation.
    transpile_compile_run(
        "data_offset_imported",
        r#"
        (module
          (import "env" "base" (global i32))
          (memory 1)
          (data (global.get 0) "\2a")
          (func (result i32) (i32.load8_u (i32.const 8))))
        "#,
        "struct Host;\n    \
         impl Imports for Host {\n        \
             fn get_global0(&self) -> i32 { 8 }\n    \
         }\n    \
         let mut inst = Instance::new(Host);\n    \
         assert_eq!(inst.func0(), 42);",
    );
}

#[test]
fn a_memory64_data_offset_may_read_an_imported_i64_global() {
    transpile_compile_run(
        "data_offset_imported64",
        r#"
        (module
          (import "env" "base" (global i64))
          (memory i64 1)
          (data (global.get 0) "\2a")
          (func (result i32) (i32.load8_u (i64.const 8))))
        "#,
        "struct Host;\n    \
         impl Imports for Host {\n        \
             fn get_global0(&self) -> i64 { 8 }\n    \
         }\n    \
         let mut inst = Instance::new(Host);\n    \
         assert_eq!(inst.func0(), 42);",
    );
}

#[test]
fn an_element_offset_may_combine_an_imported_global_with_extended_const() {
    // Slot = host's 1 + 1 = 2.
    transpile_compile_run(
        "elem_offset_imported",
        r#"
        (module
          (type $t (func (result i32)))
          (import "env" "base" (global i32))
          (table 4 funcref)
          (elem (offset (i32.add (global.get 0) (i32.const 1))) $seven)
          (func $seven (type $t) (i32.const 7))
          (func (param i32) (result i32) (call_indirect (type $t) (local.get 0))))
        "#,
        "struct Host;\n    \
         impl Imports for Host {\n        \
             fn get_global0(&self) -> i32 { 1 }\n    \
         }\n    \
         let mut inst = Instance::new(Host);\n    \
         assert_eq!(inst.func1(2), 7);\n    \
         assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| inst.func1(1))).is_err());",
    );
}

#[test]
fn a_data_offset_past_the_end_traps_at_instantiation() {
    // The host's offset leaves no room for the byte: instantiation traps.
    transpile_compile_run(
        "data_offset_imported_oob",
        r#"
        (module
          (import "env" "base" (global i32))
          (memory 1)
          (data (global.get 0) "\2a"))
        "#,
        "struct Host;\n    \
         impl Imports for Host {\n        \
             fn get_global0(&self) -> i32 { 65536 }\n    \
         }\n    \
         assert!(std::panic::catch_unwind(|| Instance::new(Host)).is_err());",
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

#[test]
fn compact_imports_with_per_item_types_keep_import_order() {
    // `(import "env" (item ..) (item ..))` (compact encoding 1): each item
    // carries its own type, and the items occupy consecutive function indices
    // 0 and 1. A regular import after the group must continue at index 2, and
    // the defined function is index 3. Each import is distinguishable (negate,
    // a constant, increment) so a swapped or dropped index changes the result:
    // neg(inc(4)) + seven() = -5 + 7 = 2.
    transpile_compile_run(
        "compact1",
        r#"
        (module
          (import "env"
            (item "neg" (func (param i32) (result i32)))
            (item "seven" (func (result i32))))
          (import "env" "inc" (func (param i32) (result i32)))
          (func (param i32) (result i32)
            local.get 0
            call 2
            call 0
            call 1
            i32.add))
        "#,
        "struct Host;\n    \
         impl Imports for Host {\n        \
             fn import0(&mut self, a0: i32) -> i32 { -a0 }\n        \
             fn import1(&mut self) -> i32 { 7 }\n        \
             fn import2(&mut self, a0: i32) -> i32 { a0 + 1 }\n    \
         }\n    \
         let mut inst = Instance::new(Host);\n    \
         assert_eq!(inst.func3(4), 2);",
    );
}

#[test]
fn compact_imports_sharing_one_type_get_separate_host_methods() {
    // `(import "env" (item ..) (item ..) (func ..))` (compact encoding 2): one
    // type shared by every name. Each name is still its own import, so the host
    // supplies two distinct methods: double(3) + triple(3) = 6 + 9 = 15.
    transpile_compile_run(
        "compact2",
        r#"
        (module
          (import "env" (item "double") (item "triple")
            (func (param i32) (result i32)))
          (func (param i32) (result i32)
            local.get 0
            call 0
            local.get 0
            call 1
            i32.add))
        "#,
        "struct Host;\n    \
         impl Imports for Host {\n        \
             fn import0(&mut self, a0: i32) -> i32 { a0 * 2 }\n        \
             fn import1(&mut self, a0: i32) -> i32 { a0 * 3 }\n    \
         }\n    \
         let mut inst = Instance::new(Host);\n    \
         assert_eq!(inst.func2(3), 15);",
    );
}
