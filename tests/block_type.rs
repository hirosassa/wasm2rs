//! Integration tests for typed block signatures (`BlockType::FuncType`): blocks
//! and `if`s that take parameters and/or produce more than one result. `loop`s
//! with parameters are covered separately (they need loop-carried variables).
//! Each module is compiled with `rustc -D warnings` and exercised.

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

    let dir = std::env::temp_dir().join(format!("wasm2rs_blockty_{test}"));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let src = dir.join("gen.rs");
    let bin = dir.join(if cfg!(windows) { "gen.exe" } else { "gen" });

    let program = format!("{generated}\nfn main() {{\n{main_body}\n}}\n");
    std::fs::write(&src, &program).expect("write generated source");

    let compile = Command::new("rustc")
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
fn block_produces_two_results_on_fallthrough() {
    // The block yields (0, 10); outside, i32.add sums them.
    transpile_compile_run(
        "block_two_results",
        r#"
        (module
          (func (param i32) (result i32)
            (block (result i32 i32)
              (local.get 0)
              (i32.const 10))
            (i32.add)))
        "#,
        "assert_eq!(func0(5), 15);",
    );
}

#[test]
fn block_consumes_a_parameter() {
    // The block takes the outer value as a parameter, multiplies it by 3.
    transpile_compile_run(
        "block_param",
        r#"
        (module
          (func (param i32) (result i32)
            (local.get 0)
            (block (param i32) (result i32)
              (i32.const 3)
              (i32.mul))))
        "#,
        "assert_eq!(func0(5), 15);",
    );
}

#[test]
fn if_takes_a_parameter_in_both_arms() {
    // The parameter (local 1) is on the stack before the `if`; both the `then`
    // and `else` arms consume it.
    transpile_compile_run(
        "if_param",
        r#"
        (module
          (func (param i32 i32) (result i32)
            (local.get 1)
            (if (param i32) (result i32) (local.get 0)
              (then (i32.const 100) (i32.add))
              (else (i32.const 200) (i32.add)))))
        "#,
        "assert_eq!(func0(1, 5), 105);\n    assert_eq!(func0(0, 5), 205);",
    );
}

#[test]
fn block_parameter_survives_a_branch_out() {
    // The block takes a parameter, adds one, then branches out carrying the
    // single result.
    transpile_compile_run(
        "block_param_br",
        r#"
        (module
          (func (param i32) (result i32)
            (local.get 0)
            (block (param i32) (result i32)
              (i32.const 1)
              (i32.add)
              (br 0))))
        "#,
        "assert_eq!(func0(5), 6);",
    );
}

#[test]
fn branch_carries_two_results_out_of_a_block() {
    // `br 0` breaks out of a two-result block carrying both values; the caller
    // sums them.
    transpile_compile_run(
        "block_br_two_results",
        r#"
        (module
          (func (result i32)
            (block (result i32 i32)
              (i32.const 3)
              (i32.const 4)
              (br 0))
            (i32.add)))
        "#,
        "assert_eq!(func0(), 7);",
    );
}

#[test]
fn if_without_else_forwards_the_parameter() {
    // A parameterised `if` with no `else`: on the false path the implicit else
    // forwards the parameter as the result.
    transpile_compile_run(
        "if_no_else_param",
        r#"
        (module
          (func (param i32 i32) (result i32)
            (local.get 1)
            (if (param i32) (result i32) (local.get 0)
              (then (i32.const 100) (i32.add)))))
        "#,
        "assert_eq!(func0(1, 5), 105);\n    assert_eq!(func0(0, 5), 5);",
    );
}

#[test]
fn branch_out_of_an_outer_two_result_block() {
    // `br 1` skips the inner block and breaks out of the outer two-result block.
    transpile_compile_run(
        "deep_br_two_results",
        r#"
        (module
          (func (result i32)
            (block (result i32 i32)
              (block
                (i32.const 3)
                (i32.const 4)
                (br 1)))
            (i32.add)))
        "#,
        "assert_eq!(func0(), 7);",
    );
}

#[test]
fn loop_branched_back_falls_through_with_two_results() {
    // A loop that is continued (via `br_if 0`) until local 0 hits zero, then
    // falls through yielding two values.
    transpile_compile_run(
        "loop_targeted_two_results",
        r#"
        (module
          (func (param i32) (result i32)
            (loop (result i32 i32)
              (local.get 0)
              (i32.const 1)
              (i32.sub)
              (local.tee 0)
              (br_if 0)
              (i32.const 10)
              (i32.const 20))
            (i32.add)))
        "#,
        "assert_eq!(func0(3), 30);",
    );
}

#[test]
fn nested_parameterised_blocks() {
    // The parameter threads through two nested parameterised blocks.
    transpile_compile_run(
        "nested_param",
        r#"
        (module
          (func (param i32) (result i32)
            (local.get 0)
            (block (param i32) (result i32)
              (block (param i32) (result i32)
                (i32.const 2)
                (i32.mul))
              (i32.const 1)
              (i32.add))))
        "#,
        "assert_eq!(func0(5), 11);",
    );
}

#[test]
fn loop_falls_through_with_two_results() {
    // A loop that is never branched back to runs once and yields two values.
    transpile_compile_run(
        "loop_two_results",
        r#"
        (module
          (func (result i32)
            (loop (result i32 i32)
              (i32.const 8)
              (i32.const 9))
            (i32.add)))
        "#,
        "assert_eq!(func0(), 17);",
    );
}
