//! Integration tests for parameterised `loop`s (`BlockType::FuncType` on a
//! `loop`). A `loop` parameter is a loop-carried value: `br` back to the loop
//! header supplies new values, so each parameter becomes a mutable variable
//! reassigned before `continue`. Each module is compiled with `rustc -D
//! warnings` and exercised.

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

    let dir = std::env::temp_dir().join(format!("wasm2rs_loopparam_{test}"));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let src = dir.join("gen.rs");
    let bin = dir.join(if cfg!(windows) { "gen.exe" } else { "gen" });

    let program = format!("{generated}\nfn main() {{\n{main_body}\n}}\n");
    std::fs::write(&src, &program).expect("write generated source");

    let compile = Command::new("rustc")
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
fn single_parameter_carries_an_accumulator() {
    // The loop parameter carries a running total; local 0 is the counter.
    // Each iteration adds 10 and `br_if 0` continues carrying the new total;
    // the final fall-through leaves it as the result.
    transpile_compile_run(
        "single_param_acc",
        r#"
        (module
          (func (param i32) (result i32)
            (i32.const 0)
            (loop (param i32) (result i32)
              (i32.const 10)
              (i32.add)
              (local.get 0)
              (i32.const 1)
              (i32.sub)
              (local.tee 0)
              (br_if 0))))
        "#,
        "assert_eq!(func0(3), 30);\n    assert_eq!(func0(1), 10);",
    );
}

#[test]
fn two_parameters_advance_together() {
    // A two-parameter loop carrying (x, y): each iteration advances to
    // (x+1, y+10) and continues while the counter (local 0) is non-zero.
    transpile_compile_run(
        "two_param",
        r#"
        (module
          (func (param i32) (result i32)
            (local i32 i32)
            (i32.const 0)
            (i32.const 0)
            (loop (param i32 i32) (result i32 i32)
              (local.set 2)
              (local.set 1)
              (local.get 1)
              (i32.const 1)
              (i32.add)
              (local.get 2)
              (i32.const 10)
              (i32.add)
              (local.get 0)
              (i32.const 1)
              (i32.sub)
              (local.tee 0)
              (br_if 0))
            (i32.add)))
        "#,
        "assert_eq!(func0(3), 33);",
    );
}

#[test]
fn two_parameters_are_swapped_each_iteration() {
    // Each iteration swaps the carried pair (a, b) -> (b, a) and continues via
    // `br_if 0` until the counter (local 0) reaches zero. Reordering the loop
    // parameters must not clobber one before the other is read.
    transpile_compile_run(
        "swap_params",
        r#"
        (module
          (func (param i32) (result i32)
            (local i32 i32)
            (i32.const 7)
            (i32.const 3)
            (loop (param i32 i32) (result i32 i32)
              (local.set 2)
              (local.set 1)
              (local.get 2)
              (local.get 1)
              (local.get 0)
              (i32.const 1)
              (i32.sub)
              (local.tee 0)
              (br_if 0))
            (i32.const 100)
            (i32.mul)
            (i32.add)))
        "#,
        // func0(3): (7,3)->(3,7)->(7,3)->(3,7) => 3 + 7*100 = 703
        // func0(2): (7,3)->(3,7)->(7,3)        => 7 + 3*100 = 307
        "assert_eq!(func0(3), 703);\n    assert_eq!(func0(2), 307);",
    );
}

#[test]
fn unconditional_branch_updates_the_loop_parameter() {
    // The loop carries an accumulator; each iteration adds 3. When the counter
    // (local 0) hits zero the empty `then` arm forwards the accumulator as the
    // loop result; otherwise the `else` arm's *unconditional* `br 1` continues
    // the loop carrying the new accumulator.
    transpile_compile_run(
        "uncond_br_param",
        r#"
        (module
          (func (param i32) (result i32)
            (i32.const 1)
            (loop (param i32) (result i32)
              (i32.const 3)
              (i32.add)
              (local.get 0)
              (i32.const 1)
              (i32.sub)
              (local.tee 0)
              (i32.eqz)
              (if (param i32) (result i32)
                (then)
                (else (br 1))))))
        "#,
        "assert_eq!(func0(1), 4);\n    assert_eq!(func0(2), 7);",
    );
}
