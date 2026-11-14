//! Tests for dropping structure that cannot affect behaviour: a block whose only
//! content is a branch to itself, and zero-initialised locals no operator names.

use crate::common;

use common::compile_run;

fn transpile(wat: &str) -> String {
    let wasm = wat::parse_str(wat).expect("valid wat");
    wasm2rs::transpile(&wasm).expect("transpile ok")
}

#[test]
fn self_branching_block_is_dropped() {
    let wat = r#"(module (func (export "f") (param i32) (result i32)
        (block (br 0))
        (block)
        (i32.add (local.get 0) (i32.const 1))))"#;
    let src = transpile(wat);

    assert!(
        !src.contains("break 'l"),
        "a block that only branches to itself should render nothing\n{src}",
    );
    assert!(
        !src.contains("'l0: {"),
        "an empty labeled block should not be emitted\n{src}",
    );
    compile_run("dead_decls_empty_block", wat, "assert_eq!(func0(4), 5);");
}

#[test]
fn block_with_result_assignment_is_kept() {
    // The value assignment before the branch is real work: the block must stay.
    let wat = r#"(module (func (export "f") (param i32) (result i32)
        (block (result i32) (br 0 (local.get 0)))))"#;
    compile_run("dead_decls_result_block", wat, "assert_eq!(func0(7), 7);");
}

#[test]
fn unreferenced_locals_get_no_declaration() {
    let wat = r#"(module (func (export "f") (param i32) (result i32)
        (local i32 i64 i32)
        (local.set 1 (local.get 0))
        (i32.add (local.get 0) (local.get 1))))"#;
    let src = transpile(wat);

    // Local 2 (i64) is never named; locals 1 and 3 (i32) are: 3 is unused too,
    // so only `l1` is declared.
    assert!(
        !src.contains("l2") && !src.contains("l3"),
        "locals no operator references should not be declared\n{src}",
    );
    assert!(src.contains("l1"), "the used local stays declared\n{src}");
    compile_run("dead_decls_unused_locals", wat, "assert_eq!(func0(3), 6);");
}

#[test]
fn locals_around_an_unreferenced_one_still_group() {
    let wat = r#"(module (func (export "f") (param i32) (result i32)
        (local i32 i64 i32)
        (local.set 1 (local.get 0))
        (local.set 3 (i32.const 5))
        (i32.add (local.get 1) (local.get 3))))"#;
    let src = transpile(wat);

    assert!(
        !src.contains("l2"),
        "the unreferenced i64 local should be dropped\n{src}",
    );
    compile_run("dead_decls_grouped", wat, "assert_eq!(func0(3), 8);");
}

#[test]
fn result_vars_of_a_region_nothing_leaves_are_dropped() {
    // Both blocks end in `unreachable` and are never branched to, so their
    // result variables are never assigned and need no declaration.
    let wat = r#"(module (func (export "f") (param i32) (result i32)
        (if (local.get 0) (then
            (drop (block (result i32) (unreachable)))
            (drop (loop (result f64) (unreachable)))))
        (i32.const 9)))"#;
    let src = transpile(wat);

    assert!(
        !src.contains("let mut v"),
        "result variables of a region no path leaves should not be declared\n{src}",
    );
    compile_run("dead_decls_dead_results", wat, "assert_eq!(func0(0), 9);");
}

#[test]
fn result_vars_of_a_targeted_block_are_kept() {
    let wat = r#"(module (func (export "f") (param i32) (result i32)
        (block (result i32) (br_if 0 (i32.const 5) (local.get 0)) (drop) (i32.const 6))))"#;
    compile_run(
        "dead_decls_live_results",
        wat,
        "assert_eq!(func0(1), 5);\n    assert_eq!(func0(0), 6);",
    );
}

#[test]
fn continuation_step_keeps_result_vars_read_by_a_dead_suspend() {
    // `suspend` after the never-left loop is dead, but the step-function lowering
    // still encodes the loop's result variable, so its declaration must stay.
    // Only the source is checked: a dead `suspend` still fails rustc for other
    // reasons (unreachable statement), which is independent of the declaration.
    let wat = r#"(module
        (elem declare func $gen)
        (type $ft (func (result i32)))
        (type $ct (cont $ft))
        (tag $yield (param i32))
        (func $gen (result i32)
            (loop (result i32) (br 0))
            suspend $yield
            i32.const 2)
        (func (export "run") (result i32)
            (resume $ct (cont.new $ct (ref.func $gen)))))"#;
    let src = transpile(wat);
    assert!(
        src.contains("let mut v0: i32 = 0;"),
        "the suspend payload reads v0, so its declaration must remain\n{src}",
    );
}
