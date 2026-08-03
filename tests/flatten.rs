//! Deeply nested control flow is emitted as a flat `loop { match pc { … } }`
//! dispatch instead of nested Rust, so its rendered nesting cannot overflow
//! rustc's recursive-descent parser. These tests pin that a function past the
//! nesting threshold (a) flattens (its output carries the dispatch markers and
//! its indentation stays bounded, not proportional to the wasm depth) and
//! (b) still computes the same result once compiled and run.

mod common;

use common::compile_run;

/// The deepest indentation (leading spaces) of any line in `source`.
fn max_indent(source: &str) -> usize {
    source
        .lines()
        .map(|line| line.len() - line.trim_start().len())
        .max()
        .unwrap_or(0)
}

/// A function whose body nests `depth` `block`s, each incrementing a local by
/// one and guarding a never-taken `br_if` (so every block is a real branch
/// target). The function returns the accumulated count, i.e. `depth`.
fn nested_increment_blocks(depth: usize) -> String {
    let mut body = String::new();
    for _ in 0..depth {
        body.push_str(
            "(block (local.set 0 (i32.add (local.get 0) (i32.const 1))) (br_if 0 (i32.const 0))\n",
        );
    }
    body.push_str(&")".repeat(depth));
    format!("(module (func (export \"f\") (result i32) (local i32)\n{body}\n(local.get 0)))")
}

#[test]
fn deeply_nested_blocks_flatten_and_run() {
    let depth = 50; // > FLATTEN_DEPTH_THRESHOLD (40)
    let wat = nested_increment_blocks(depth);
    let wasm = wat::parse_str(&wat).expect("valid wat");
    let source = wasm2rs::transpile(&wasm).expect("transpile ok");

    // The deep function is flattened, not rendered as nested labelled loops.
    assert!(
        source.contains("match pc {"),
        "expected a flat dispatch loop:\n{source}"
    );
    // Nested rendering would indent ~4 spaces per level (~200 for depth 50); the
    // flat form's nesting is a small constant regardless of wasm depth.
    assert!(
        max_indent(&source) < 40,
        "flat output should have bounded indentation, got {}",
        max_indent(&source)
    );

    compile_run(
        "flatten_blocks",
        &wat,
        &format!("assert_eq!(func0(), {depth});"),
    );
}

/// A countdown loop (`sum = depth + (depth-1) + … + 1`) wrapped in `depth`
/// `block`s so the whole function nests past the flatten threshold while still
/// exercising a `loop`, an unconditional `br` (continue) and a value-carrying
/// `br_if` inside the flat dispatch.
fn wrapped_countdown_loop(depth: usize) -> String {
    let inner = "\
(block $done
  (loop $lp
    (br_if $done (i32.eqz (local.get $i)))
    (local.set $sum (i32.add (local.get $sum) (local.get $i)))
    (local.set $i (i32.sub (local.get $i) (i32.const 1)))
    (br $lp)))";
    let open = "(block ".repeat(depth);
    let close = ")".repeat(depth);
    format!(
        "(module (func (export \"g\") (result i32) (local $i i32) (local $sum i32)\n\
         (local.set $i (i32.const {depth}))\n\
         {open}{inner}{close}\n\
         (local.get $sum)))"
    )
}

#[test]
fn deep_wrapped_loop_flattens_and_runs() {
    let depth = 45; // > FLATTEN_DEPTH_THRESHOLD (40)
    let wat = wrapped_countdown_loop(depth);
    let wasm = wat::parse_str(&wat).expect("valid wat");
    let source = wasm2rs::transpile(&wasm).expect("transpile ok");

    assert!(
        source.contains("match pc {"),
        "expected a flat dispatch loop:\n{source}"
    );
    assert!(max_indent(&source) < 40, "flat output should stay bounded");

    let expected = depth * (depth + 1) / 2; // 45 + 44 + … + 1 = 1035
    compile_run(
        "flatten_loop",
        &wat,
        &format!("assert_eq!(func0(), {expected});"),
    );
}

/// A function nesting `depth` `if`/`else`s in the (always-taken) `then` arm,
/// each incrementing a local; the `else` arm (never taken) decrements. Returns
/// the accumulated count, i.e. `depth`.
fn nested_if_else(depth: usize) -> String {
    let mut inner = String::new();
    for _ in 0..depth {
        inner = format!(
            "(if (i32.const 1) \
               (then (local.set 0 (i32.add (local.get 0) (i32.const 1))) {inner}) \
               (else (local.set 0 (i32.sub (local.get 0) (i32.const 1)))))"
        );
    }
    format!("(module (func (export \"f\") (result i32) (local i32)\n{inner}\n(local.get 0)))")
}

#[test]
fn deeply_nested_if_else_flattens_and_runs() {
    let depth = 50; // > FLATTEN_DEPTH_THRESHOLD (40)
    let wat = nested_if_else(depth);
    let wasm = wat::parse_str(&wat).expect("valid wat");
    let source = wasm2rs::transpile(&wasm).expect("transpile ok");

    assert!(
        source.contains("match pc {"),
        "expected a flat dispatch loop:\n{source}"
    );
    assert!(max_indent(&source) < 40, "flat output should stay bounded");

    compile_run(
        "flatten_if_else",
        &wat,
        &format!("assert_eq!(func0(), {depth});"),
    );
}

/// A `br_table` switch (selector 0 → 10, otherwise → 20) wrapped in `depth`
/// `block`s so the function nests past the flatten threshold while exercising a
/// `br_table` inside the flat dispatch.
fn wrapped_br_table(depth: usize) -> String {
    let inner = "(block $a (block $b (br_table $a $b (local.get 0))) \
                 (return (i32.const 20))) (return (i32.const 10))";
    let open = "(block ".repeat(depth);
    let close = ")".repeat(depth);
    format!("(module (func (export \"sw\") (param i32) (result i32)\n{open}{inner}{close}))")
}

#[test]
fn deep_br_table_flattens_and_runs() {
    let wat = wrapped_br_table(45); // 45 + 2 nested > FLATTEN_DEPTH_THRESHOLD (40)
    let wasm = wat::parse_str(&wat).expect("valid wat");
    let source = wasm2rs::transpile(&wasm).expect("transpile ok");

    assert!(
        source.contains("match pc {"),
        "expected a flat dispatch loop:\n{source}"
    );
    assert!(max_indent(&source) < 40, "flat output should stay bounded");

    compile_run(
        "flatten_br_table",
        &wat,
        "assert_eq!(func0(0), 10);\nassert_eq!(func0(1), 20);\nassert_eq!(func0(7), 20);",
    );
}

#[test]
fn very_deep_function_compiles_with_default_stack() {
    // A nesting depth far past what rustc's recursive-descent parser tolerates
    // as nested Rust (deeply nested `loop`s overflow its stack — see RESEARCH.md
    // Phase 4, which hit a SIGBUS on the real googlesql module). Flattened, the
    // rendered nesting is a small constant, so `rustc` compiles it with its
    // default stack (the `compile_run` below uses a plain `rustc`). This is the
    // permanent fix the flattening was built for.
    //
    // The transpiler side still recurses on the input's nesting (the `wat`
    // parser and the flat lowering), which a real `.wasm` binary reaches through
    // the iterative `wasmparser`; here the deep `wat` is parsed on a large-stack
    // thread so the test does not depend on `RUST_MIN_STACK`.
    let handle = std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(|| {
            let depth = 1500;
            let wat = nested_increment_blocks(depth);
            let wasm = wat::parse_str(&wat).expect("valid wat");
            let source = wasm2rs::transpile(&wasm).expect("transpile ok");
            assert!(max_indent(&source) < 40, "flat output should stay bounded");
            compile_run(
                "flatten_very_deep",
                &wat,
                &format!("assert_eq!(func0(), {depth});"),
            );
        })
        .expect("spawn worker thread");
    handle.join().expect("deep transpile + compile succeeds");
}

#[test]
fn shallow_function_stays_nested() {
    // A modestly nested function keeps its readable nested form.
    let wat = nested_increment_blocks(3);
    let wasm = wat::parse_str(&wat).expect("valid wat");
    let source = wasm2rs::transpile(&wasm).expect("transpile ok");

    assert!(
        !source.contains("match pc {"),
        "a shallow function should not be flattened:\n{source}"
    );
    compile_run("flatten_shallow", &wat, "assert_eq!(func0(), 3);");
}
