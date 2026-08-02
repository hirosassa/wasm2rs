//! Integration tests for value-carrying `br_table` targets: a `br_table` whose
//! targets are result-bearing blocks or parameterised loops. Each arm assigns
//! the carried operands to that target's variables before the `break`/`continue`.
//! Every module is compiled with `rustc -D warnings` and exercised.

use std::process::Command;

fn transpile_compile_run(test: &str, wat: &str, main_body: &str) {
    let wasm = wat::parse_str(wat).expect("valid wat");
    let generated = wasm2rs::transpile(&wasm).expect("transpile ok");

    let dir = std::env::temp_dir().join(format!("wasm2rs_brtblval_{test}"));
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
fn block_result_carried_out_via_br_table() {
    // A `br_table` inside a result-bearing inner block: selector 0 breaks the
    // inner block (carrying 10 as its result, which is then +1), the default
    // breaks the outer block directly (carrying 10 as its result, skipping +1).
    transpile_compile_run(
        "block_result",
        r#"
        (module
          (func (param i32) (result i32)
            (block (result i32)
              (block (result i32)
                (i32.const 10)
                (local.get 0)
                (br_table 0 1))
              (i32.const 1)
              (i32.add))))
        "#,
        "assert_eq!(func0(0), 11);\n    assert_eq!(func0(1), 10);",
    );
}

#[test]
fn loop_param_carried_in_via_br_table() {
    // A `br_table` mixing a loop-continue and a block-break of the same arity.
    // The loop carries an accumulator; each iteration adds 5 and decrements the
    // counter (local 0). When the counter hits zero the selector picks the outer
    // block (break, carrying the accumulator as the result); otherwise it picks
    // the loop (continue, carrying the accumulator as the new loop parameter).
    transpile_compile_run(
        "loop_param",
        r#"
        (module
          (func (param i32) (result i32)
            (block (result i32)
              (i32.const 0)
              (loop (param i32) (result i32)
                (i32.const 5)
                (i32.add)
                (local.get 0)
                (i32.const 1)
                (i32.sub)
                (local.tee 0)
                (br_table 1 0)))))
        "#,
        "assert_eq!(func0(3), 15);\n    assert_eq!(func0(1), 5);",
    );
}

#[test]
fn two_results_carried_out_via_br_table() {
    // A `br_table` carrying two values out of nested two-result blocks. Selector
    // 0 breaks the inner block (its results (20,5) are then advanced to (20,105)
    // before becoming the outer results); the default breaks the outer block
    // directly, carrying (20,5). The function returns a - b of the outer results.
    transpile_compile_run(
        "two_results",
        r#"
        (module
          (func (param i32) (result i32)
            (block (result i32 i32)
              (block (result i32 i32)
                (i32.const 20)
                (i32.const 5)
                (local.get 0)
                (br_table 0 1))
              (i32.const 100)
              (i32.add))
            (i32.sub)))
        "#,
        "assert_eq!(func0(0), -85);\n    assert_eq!(func0(1), 15);",
    );
}
