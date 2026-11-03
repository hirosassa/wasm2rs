//! A `br`/`br_if`/`br_table` whose target is the function's own (implicit)
//! outermost label behaves like a `return` of the function's results.

use crate::common;

#[test]
fn br_to_the_function_label_returns_its_value() {
    common::compile_run(
        "brfn_br",
        r#"(module (func (export "f") (result i32) (br 0 (i32.const 7))))"#,
        "assert_eq!(func0(), 7);",
    );
}

#[test]
fn br_if_to_the_function_label_with_a_value() {
    common::compile_run(
        "brfn_br_if_value",
        r#"(module (func (export "f") (param i32) (result i32)
             (drop (br_if 0 (i32.const 10) (local.get 0)))
             (i32.const 11)))"#,
        "assert_eq!(func0(1), 10);\n assert_eq!(func0(0), 11);",
    );
}

#[test]
fn br_if_to_the_function_label_without_a_value() {
    // Taking the branch skips the store.
    common::compile_run(
        "brfn_br_if_void",
        r#"(module (memory 1)
             (func (export "f") (param i32)
               (br_if 0 (local.get 0))
               (i32.store (i32.const 0) (i32.const 5)))
             (func (export "get") (result i32) (i32.load (i32.const 0))))"#,
        "let mut inst = Instance::new();\n inst.func0(1);\n assert_eq!(inst.func1(), 0);\n \
         inst.func0(0);\n assert_eq!(inst.func1(), 5);",
    );
}

#[test]
fn br_table_mixing_the_function_label_and_a_block() {
    // Index 0 -> the function label (returns 5); anything else -> the block.
    common::compile_run(
        "brfn_br_table",
        r#"(module (func (export "f") (param i32) (result i32)
             (block (result i32) (br_table 1 0 (i32.const 5) (local.get 0)))
             (i32.const 1) (i32.add)))"#,
        "assert_eq!(func0(0), 5);\n assert_eq!(func0(1), 6);\n assert_eq!(func0(9), 6);",
    );
}

#[test]
fn br_out_of_a_loop_to_the_function_label() {
    // Counts down; the function label (depth 1 from inside the loop) returns it.
    common::compile_run(
        "brfn_loop",
        r#"(module (func (export "f") (param i32) (result i32) (local i32)
             (loop
               (local.set 1 (i32.add (local.get 1) (i32.const 1)))
               (br_if 1 (local.get 1) (i32.eq (local.get 1) (local.get 0)))
               (br 0))
             (unreachable)))"#,
        "assert_eq!(func0(3), 3);",
    );
}

#[test]
fn functions_without_such_a_branch_are_unchanged() {
    // No implicit function frame is introduced for ordinary control flow.
    let wasm = wat::parse_str(
        r#"(module (func (export "f") (param i32) (result i32)
             (block (result i32) (br_if 0 (i32.const 1) (local.get 0)) (drop) (i32.const 2))))"#,
    )
    .unwrap();
    let out = wasm2rs::transpile(&wasm).unwrap();
    assert!(
        !out.contains("'l1"),
        "unexpected extra region label:\n{out}"
    );
}

#[test]
fn br_to_the_function_label_from_a_deeply_nested_flattened_function() {
    // 50 nested blocks exceed the flatten threshold, so the body is lowered to
    // the state machine; the innermost `br` reaches the function label.
    let depth = 50;
    let open = "(block ".repeat(depth);
    let close = ")".repeat(depth);
    let wat = format!(
        r#"(module (func (export "f") (param i32) (result i32)
             {open} (br_if {depth} (i32.const 42) (local.get 0)) {close}
             (i32.const 7)))"#
    );
    common::compile_run(
        "brfn_flattened",
        &wat,
        "assert_eq!(func0(1), 42);\n assert_eq!(func0(0), 7);",
    );
}

#[test]
fn br_to_the_function_label_from_a_try_body() {
    common::compile_run(
        "brfn_try_body",
        r#"(module (func (export "f") (param i32) (result i32)
             try (result i32)
               i32.const 3
               local.get 0
               br_if 1
               drop
               i32.const 5
             catch_all
               i32.const 6
             end
             drop
             i32.const 9))"#,
        "assert_eq!(func0(1), 3);\n assert_eq!(func0(0), 9);",
    );
}

#[test]
fn br_to_the_function_label_from_a_catch_handler() {
    common::compile_run(
        "brfn_try_handler",
        r#"(module (tag $e)
             (func (export "f") (result i32)
               try (result i32)
                 throw $e
               catch_all
                 i32.const 8
                 br 1
               end
               drop
               i32.const 9))"#,
        "assert_eq!(func0(), 8);",
    );
}
