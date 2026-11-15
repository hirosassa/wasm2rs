//! Tests for the opt-in `TranspileOptions::call_depth_limit`.
//!
//! Unbounded guest recursion otherwise exhausts the native stack and aborts the
//! host process, which no `catch_unwind` can intercept. With a limit, every defined
//! function counts its activation and the call that would exceed the limit panics
//! with `call stack exhausted` (a trap) instead. The option is off by default, so
//! the plain output must not mention the counter at all.

use std::process::Command;
use wasm2rs::TranspileOptions;

use crate::common;

fn transpile_limited_with(wat: &str, limit: u32, typed: bool) -> String {
    let wasm = wat::parse_str(wat).expect("valid wat");
    wasm2rs::transpile_with_options(
        &wasm,
        &TranspileOptions::default()
            .with_call_depth_limit(Some(limit))
            .with_typed_traps(typed),
    )
    .expect("transpile ok")
}

fn transpile_limited(wat: &str, limit: u32) -> String {
    transpile_limited_with(wat, limit, false)
}

/// Transpile `wat` with the limit, run `main_body`, and assert it exits 0, once with
/// untyped traps (a string panic) and once with `typed_traps` (a `Wasm2RsTrap`).
fn compile_run_limited(name: &str, wat: &str, limit: u32, main_body: &str) {
    for typed in [false, true] {
        let typed_payload = if typed {
            "if let Some(t) = payload.downcast_ref::<Wasm2RsTrap>() {
        assert_eq!(*t, Wasm2RsTrap::CallStackExhausted);
        return;
    }"
        } else {
            ""
        };
        let main_body = main_body.replace("/*TYPED_PAYLOAD*/", typed_payload);
        let generated = transpile_limited_with(wat, limit, typed);
        let program = format!("{generated}\nfn main() {{\n{main_body}\n}}\n");
        let bin = common::build(&format!("{name}_typed_{typed}"), &program);
        let run = Command::new(&bin).status().expect("run generated binary");
        assert!(
            run.success(),
            "generated program assertions failed:\n{name} (typed_traps: {typed})"
        );
    }
}

/// Runs `f` and asserts it panics with `call stack exhausted`, as a string panic or,
/// under `typed_traps`, as the matching `Wasm2RsTrap` (the placeholder is filled in
/// by `compile_run_limited`).
const EXPECT_EXHAUSTED: &str = r#"
fn expect_exhausted<R>(f: impl FnOnce() -> R) {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    std::panic::set_hook(prev);
    let payload = match r {
        Err(p) => p,
        Ok(_) => panic!("expected call stack exhausted"),
    };
    /*TYPED_PAYLOAD*/
    let msg = payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_default();
    assert!(msg.contains("call stack exhausted"), "unexpected trap: {msg}");
}
"#;

const RUNAWAY: &str = r#"(module
    (func $runaway (export "runaway") (call $runaway))
    (func $down (export "down") (param i32) (result i32)
      local.get 0
      i32.eqz
      if (result i32)
        i32.const 7
      else
        local.get 0
        i32.const 1
        i32.sub
        call $down
      end))"#;

#[test]
fn option_is_off_by_default_and_leaves_output_untouched() {
    let wasm = wat::parse_str(RUNAWAY).expect("valid wat");
    let plain = wasm2rs::transpile(&wasm).expect("transpile ok");
    assert!(!plain.contains("CallDepth"), "{plain}");
    assert!(transpile_limited(RUNAWAY, 100).contains("CallDepth"));
}

#[test]
fn runaway_recursion_traps_instead_of_aborting() {
    compile_run_limited(
        "depth_runaway",
        RUNAWAY,
        1000,
        &format!(
            "{EXPECT_EXHAUSTED}
            expect_exhausted(|| func0());
            // The guard unwound the counter: a recursion below the limit still works.
            assert_eq!(func1(500), 7);
            expect_exhausted(|| func0());
            assert_eq!(func1(900), 7);"
        ),
    );
}

#[test]
fn recursion_beyond_the_limit_traps_but_below_it_runs() {
    compile_run_limited(
        "depth_boundary",
        RUNAWAY,
        100,
        &format!(
            "{EXPECT_EXHAUSTED}
            assert_eq!(func1(50), 7);
            expect_exhausted(|| func1(500));"
        ),
    );
}

#[test]
fn mutual_recursion_through_call_indirect_traps() {
    compile_run_limited(
        "depth_indirect",
        r#"(module
            (type $t (func))
            (table funcref (elem $a $b))
            (func $a (export "a") (call_indirect (type $t) (i32.const 1)))
            (func $b (call_indirect (type $t) (i32.const 0))))"#,
        500,
        &format!(
            "{EXPECT_EXHAUSTED}
            let mut inst = Instance::new();
            expect_exhausted(|| inst.func0());"
        ),
    );
}

#[test]
fn stateful_module_counts_method_calls() {
    compile_run_limited(
        "depth_stateful",
        r#"(module
            (memory 1)
            (func $r (export "r") (i32.store (i32.const 0) (i32.const 1)) (call $r)))"#,
        300,
        &format!(
            "{EXPECT_EXHAUSTED}
            let mut inst = Instance::new();
            expect_exhausted(|| inst.func0());"
        ),
    );
}

#[test]
fn tail_calls_do_not_grow_the_depth() {
    // A million trampolined / looped hops under a limit of 50: the depth must stay
    // constant, so the limit is never reached and the counter is not corrupted.
    compile_run_limited(
        "depth_tail_calls",
        r#"(module
            (func $is_even (export "is_even") (param i32) (result i32)
              local.get 0
              i32.eqz
              if (result i32)
                i32.const 1
              else
                local.get 0
                i32.const 1
                i32.sub
                return_call $is_odd
              end)
            (func $is_odd (param i32) (result i32)
              local.get 0
              i32.eqz
              if (result i32)
                i32.const 0
              else
                local.get 0
                i32.const 1
                i32.sub
                return_call $is_even
              end)
            (func $sum (export "sum") (param i64 i64) (result i64)
              local.get 0
              i64.eqz
              if (result i64)
                local.get 1
              else
                local.get 0
                i64.const 1
                i64.sub
                local.get 1
                local.get 0
                i64.add
                return_call $sum
              end)
            (func $runaway (export "runaway") (call $runaway)))"#,
        50,
        &format!(
            "{EXPECT_EXHAUSTED}
            assert_eq!(func0(1_000_000), 1);
            assert_eq!(func0(1_000_001), 0);
            assert_eq!(func2(1_000_000, 0), 500_000_500_000);
            // The counter is intact: a runaway still traps afterwards.
            expect_exhausted(|| func3());"
        ),
    );
}

#[test]
fn a_non_tail_call_in_a_tail_calling_function_still_counts() {
    // `f` tail-calls `g` (trampolined) but also recurses non-tail: the runaway
    // non-tail recursion must trap.
    compile_run_limited(
        "depth_tail_mixed",
        r#"(module
            (func $f (export "f") (param i32) (result i32)
              local.get 0
              i32.eqz
              if (result i32)
                i32.const 0
              else
                local.get 0
                call $f
                drop
                local.get 0
                return_call $g
              end)
            (func $g (param i32) (result i32) local.get 0))"#,
        200,
        &format!(
            "{EXPECT_EXHAUSTED}
            assert_eq!(func0(0), 0);
            expect_exhausted(|| func0(1));"
        ),
    );
}
