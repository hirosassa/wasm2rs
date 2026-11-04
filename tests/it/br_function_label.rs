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
    // Ordinary control flow gets no implicit function frame: the output is the
    // same as before this feature (one region, `'l0`, for the explicit block).
    let wasm = wat::parse_str(
        r#"(module (func (export "f") (param i32) (result i32)
             (block (result i32) (br_if 0 (i32.const 1) (local.get 0)) (drop) (i32.const 2))))"#,
    )
    .unwrap();
    let out = wasm2rs::transpile(&wasm).unwrap();
    // Skip the lint-suppression attribute line; compare the function itself.
    let function = out.split_once('\n').unwrap().1.trim();
    assert_eq!(
        function,
        "pub fn func0(l0: i32) -> i32 {\n    let mut v0: i32 = 0;\n    'l0: {\n        \
         if l0 != 0 {\n            v0 = 1i32;\n            break 'l0;\n        }\n        \
         v0 = 2i32;\n    }\n    v0\n}"
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

#[test]
fn a_resume_handler_label_may_target_the_function_label() {
    // `(on $yield 0)` at region depth 0 branches to the function's own label, so
    // the suspended continuation and its yielded value are the function's results.
    common::compile_run(
        "brfn_resume_label",
        r#"(module
             (type $ft (func (result i32)))
             (type $ct (cont $ft))
             (tag $yield (param i32))
             (func $gen (result i32) i32.const 10 suspend $yield i32.const 30)
             (func (export "f") (result i32 (ref null $ct))
               ref.func $gen cont.new $ct
               resume $ct (on $yield 0)
               ref.null $ct))"#,
        "let mut inst = Instance::new();\n let (v, k) = inst.func1();\n \
         assert_eq!(v, 10);\n assert_ne!(k, u32::MAX);",
    );
}
