//! Integration tests for Phase 6f: multi-value function results. A function
//! returning N values maps to a Rust tuple `(T0, .., Tn)`; callers destructure
//! it back onto the operand stack. Each module is compiled with
//! `rustc -D warnings` and exercised.

use std::process::Command;

fn transpile_compile_run(test: &str, wat: &str, main_body: &str) {
    let wasm = wat::parse_str(wat).expect("valid wat");
    let generated = wasm2rs::transpile(&wasm).expect("transpile ok");

    let dir = std::env::temp_dir().join(format!("wasm2rs_mv_{test}"));
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
fn function_returns_a_pair() {
    // Swap: pushes arg1 then arg0, so the result tuple is (l1, l0).
    transpile_compile_run(
        "pair",
        r#"
        (module
          (func (param i32 i32) (result i32 i32)
            (local.get 1) (local.get 0)))
        "#,
        "assert_eq!(func0(3, 7), (7, 3));",
    );
}

#[test]
fn function_returns_three_mixed_values() {
    transpile_compile_run(
        "triple",
        r#"
        (module
          (func (result i32 i64 f32)
            (i32.const 1) (i64.const 2) (f32.const 3.5)))
        "#,
        "assert_eq!(func0(), (1, 2i64, 3.5f32));",
    );
}

#[test]
fn call_destructures_multi_value_result() {
    // func0 returns (x, 2x); func1 calls it and adds the two results: 3x.
    transpile_compile_run(
        "call",
        r#"
        (module
          (func (param i32) (result i32 i32)
            (local.get 0) (i32.add (local.get 0) (local.get 0)))
          (func (param i32) (result i32)
            (call 0 (local.get 0))
            (i32.add)))
        "#,
        "assert_eq!(func1(5), 15);",
    );
}

#[test]
fn call_indirect_destructures_multi_value_result() {
    // Table slot 0 is func0 returning (x, x*x); func1 dispatches and adds them.
    transpile_compile_run(
        "indirect",
        r#"
        (module
          (type $t (func (param i32) (result i32 i32)))
          (table 1 funcref)
          (elem (i32.const 0) 0)
          (func (param i32) (result i32 i32)
            (local.get 0) (i32.mul (local.get 0) (local.get 0)))
          (func (param i32) (result i32)
            (call_indirect (type $t) (local.get 0) (i32.const 0))
            (i32.add)))
        "#,
        "let mut inst = Instance::new();\n    assert_eq!(inst.func1(4), 20);",
    );
}
