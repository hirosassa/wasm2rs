//! Integration tests for Phase 4a: direct calls (`call`). Each test transpiles
//! a module, compiles the generated Rust with `rustc -D warnings`, and runs
//! assertions so the call behaviour (arguments, results, recursion, and
//! free-fn vs `&mut self` method dispatch) is verified end to end.

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

    let dir = std::env::temp_dir().join(format!("wasm2rs_calls_{test}"));
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
fn direct_call_passes_arguments_and_result() {
    transpile_compile_run(
        "direct",
        r#"
        (module
          (func (param i32 i32) (result i32) (i32.add (local.get 0) (local.get 1)))
          (func (param i32) (result i32) (call 0 (local.get 0) (i32.const 10))))
        "#,
        "assert_eq!(func1(5), 15);\n    assert_eq!(func1(-4), 6);",
    );
}

#[test]
fn recursive_call_sums_to_n() {
    transpile_compile_run(
        "recursion",
        r#"
        (module
          (func (param i32) (result i32)
            (if (result i32) (i32.eqz (local.get 0))
              (then (i32.const 0))
              (else (i32.add (local.get 0)
                             (call 0 (i32.sub (local.get 0) (i32.const 1))))))))
        "#,
        "assert_eq!(func0(5), 15);\n    assert_eq!(func0(0), 0);",
    );
}

#[test]
fn mutual_recursion_parity() {
    transpile_compile_run(
        "mutual",
        r#"
        (module
          (func (param i32) (result i32)
            (if (result i32) (i32.eqz (local.get 0))
              (then (i32.const 1))
              (else (call 1 (i32.sub (local.get 0) (i32.const 1))))))
          (func (param i32) (result i32)
            (if (result i32) (i32.eqz (local.get 0))
              (then (i32.const 0))
              (else (call 0 (i32.sub (local.get 0) (i32.const 1)))))))
        "#,
        "assert_eq!(func0(10), 1);\n    \
         assert_eq!(func1(10), 0);\n    \
         assert_eq!(func0(7), 0);\n    \
         assert_eq!(func1(7), 1);",
    );
}

#[test]
fn operand_keeps_pre_call_value_when_callee_mutates_global() {
    // The left operand of the `add` reads global 0 *before* the call, and the
    // callee overwrites that global. Correct spill-before-call must freeze the
    // pre-call value (100), so the result is 100 + (100 + 1) = 201, not 101.
    transpile_compile_run(
        "spill",
        r#"
        (module
          (global (mut i32) (i32.const 100))
          (func (param i32) (result i32)
            (global.set 0 (i32.const 0))
            (i32.add (local.get 0) (i32.const 1)))
          (func (result i32)
            (i32.add (global.get 0) (call 0 (global.get 0)))))
        "#,
        "let mut inst = Instance::new();\n    assert_eq!(inst.func1(), 201);",
    );
}

#[test]
fn void_call_in_stateful_module_dispatches_on_self() {
    // A module with a global becomes a `struct Instance`; the caller must
    // reach the callee through `self`, and the void call runs for its effect.
    transpile_compile_run(
        "stateful",
        r#"
        (module
          (global (mut i32) (i32.const 0))
          (func (param i32)
            (global.set 0 (i32.add (global.get 0) (local.get 0))))
          (func (param i32) (result i32)
            (call 0 (local.get 0))
            (global.get 0)))
        "#,
        "let mut inst = Instance::new();\n    \
         assert_eq!(inst.func1(5), 5);\n    \
         assert_eq!(inst.func1(3), 8);",
    );
}
