//! Tests for the opt-in `TranspileOptions::call_depth_limit`.
//!
//! Unbounded guest recursion otherwise exhausts the native stack and aborts the
//! host process, which no `catch_unwind` can intercept. With a limit, every defined
//! function counts its activation and the call that would exceed the limit panics
//! with `call stack exhausted` (a trap) instead. The option is off by default, so
//! the plain output must not mention the counter at all.
//!
//! Only functions that can lie on a call-graph cycle carry the guard (every cycle
//! needs one for the recursion to trap); the placement tests count the emitted
//! `CallDepth::enter(` call sites to pin which functions those are.

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

/// How many functions of the transpiled output carry the guard.
fn guard_sites(src: &str) -> usize {
    src.matches("CallDepth::enter(").count()
}

#[test]
fn leaf_and_acyclic_functions_carry_no_guard() {
    // `$leaf` and `$top` (a caller of a leaf) are on no cycle; only the
    // self-recursive `$rec` needs the counter.
    let wat = r#"(module
        (func $leaf (result i32) i32.const 1)
        (func $top (export "top") (result i32) call $leaf call $leaf i32.add)
        (func $rec (export "rec") (call $rec)))"#;
    assert_eq!(guard_sites(&transpile_limited(wat, 100)), 1);
    compile_run_limited(
        "depth_acyclic",
        wat,
        100,
        &format!(
            "{EXPECT_EXHAUSTED}
            // Unguarded functions never trap, even at a limit they could not fit.
            assert_eq!(func1(), 2);
            expect_exhausted(|| func2());
            assert_eq!(func1(), 2);"
        ),
    );
}

#[test]
fn a_module_without_cycles_has_no_guard_at_all() {
    let wat = r#"(module
        (func $a (export "a") (result i32) call $b)
        (func $b (result i32) call $c)
        (func $c (result i32) i32.const 3))"#;
    let src = transpile_limited(wat, 1);
    assert_eq!(guard_sites(&src), 0, "{src}");
    // The helper is not emitted either, since nothing uses it.
    assert!(!src.contains("CALL_DEPTH"), "{src}");
}

#[test]
fn one_guard_covers_a_whole_cycle_and_still_traps() {
    // a -> b -> c -> a: a single head breaks the cycle.
    let wat = r#"(module
        (func $a (export "a") (call $b))
        (func $b (call $c))
        (func $c (call $a)))"#;
    assert_eq!(guard_sites(&transpile_limited(wat, 100)), 1);
    compile_run_limited(
        "depth_cycle3",
        wat,
        100,
        &format!(
            "{EXPECT_EXHAUSTED}
            expect_exhausted(|| func0());
            expect_exhausted(|| func0());"
        ),
    );
}

#[test]
fn indirect_calls_only_reach_functions_of_their_type() {
    // `$f` (i64 -> i32, exported) calls through type `$u` (i32 -> i32); the only
    // `$u` function in the table is `$g`, which does not call back, so there is no
    // cycle through `$f`. `$h` and `$f_v` form a real cycle through type `$v`.
    let acyclic = r#"(module
        (type $u (func (param i32) (result i32)))
        (type $v (func))
        (table funcref (elem $g $h))
        (func $g (type $u) local.get 0)
        (func $h (type $v) (call $f_v))
        (func $f_v (type $v) (call_indirect (type $v) (i32.const 1)))
        (func $f (export "f") (param i64) (result i32)
          local.get 0 i32.wrap_i64 i32.const 0 call_indirect (type $u)))"#;
    assert_eq!(guard_sites(&transpile_limited(acyclic, 100)), 1);
    let none = r#"(module
        (type $u (func (param i32) (result i32)))
        (table funcref (elem $g))
        (func $g (type $u) local.get 0)
        (func $f (export "f") (param i64) (result i32)
          local.get 0 i32.wrap_i64 i32.const 0 call_indirect (type $u)))"#;
    assert_eq!(guard_sites(&transpile_limited(none, 100)), 0);
    // Being exported does not make a function a funcref a running guest can reach:
    // `$f` has the called type but is in no table, so `$f` -> dispatch -> `$g` ends.
    let exported = r#"(module
        (type $u (func (param i32) (result i32)))
        (table funcref (elem $g))
        (func $g (type $u) local.get 0)
        (func $f (export "f") (type $u)
          local.get 0 i32.const 0 call_indirect (type $u)))"#;
    assert_eq!(guard_sites(&transpile_limited(exported, 100)), 0);
    // Naming it in the table (or by `ref.func`) does.
    let named = exported.replace("(elem $g)", "(elem $g $f)");
    assert_eq!(guard_sites(&transpile_limited(&named, 100)), 1);
}

#[test]
fn a_cycle_through_an_indirect_call_is_guarded_once_at_the_dispatch() {
    let wat = r#"(module
        (type $t (func))
        (table funcref (elem $a $b $leaf))
        (func $a (export "a") (call_indirect (type $t) (i32.const 1)))
        (func $b (call_indirect (type $t) (i32.const 0)))
        (func $leaf))"#;
    // Every indirect call of the type goes through the shared `call_ref_t{ti}`
    // dispatch method, so one counter there covers the whole cycle and neither
    // caller needs its own.
    let src = transpile_limited(wat, 100);
    assert_eq!(guard_sites(&src), 1, "{src}");
    let dispatch = src
        .split("pub fn call_ref_t")
        .nth(1)
        .expect("a dispatch method");
    assert!(
        dispatch
            .lines()
            .nth(1)
            .is_some_and(|l| l.contains("CallDepth::enter(100)")),
        "{dispatch}"
    );
}

#[test]
fn a_direct_cycle_next_to_indirect_calls_still_gets_its_own_guard() {
    // `$r` recurses directly, with no indirect call involved; the dispatch counter
    // cannot see it.
    let wat = r#"(module
        (type $t (func))
        (table funcref (elem $leaf))
        (func $leaf)
        (func $r (export "r") (call $r) (call_indirect (type $t) (i32.const 0))))"#;
    // $r is on a direct cycle and the dispatch node is on no cycle (its only target
    // is a leaf), so exactly one guard, in `$r`.
    let src = transpile_limited(wat, 100);
    assert_eq!(guard_sites(&src), 1, "{src}");
    compile_run_limited(
        "depth_direct_beside_indirect",
        wat,
        100,
        &format!(
            "{EXPECT_EXHAUSTED}
            let mut inst = Instance::new();
            expect_exhausted(|| inst.func1());"
        ),
    );
}

#[test]
fn tail_calls_through_a_guarded_dispatch_do_not_grow_the_depth() {
    // A million `return_call_indirect` hops under a limit of 50, with the dispatch
    // on a cycle (so it is guarded): the trampolined `_tc` dispatch must keep the
    // depth constant.
    compile_run_limited(
        "depth_tail_indirect",
        r#"(module
            (type $t (func (param i32) (result i32)))
            (table funcref (elem $count))
            (func $count (type $t) (param i32) (result i32)
              local.get 0
              i32.eqz
              if (result i32)
                i32.const 9
              else
                local.get 0 i32.const 1 i32.sub
                i32.const 0
                return_call_indirect (type $t)
              end)
            (func $runaway (export "runaway") (call $runaway)))"#,
        50,
        &format!(
            "{EXPECT_EXHAUSTED}
            let mut inst = Instance::new();
            assert_eq!(inst.func0(1_000_000), 9);
            expect_exhausted(|| inst.func1());"
        ),
    );
}

#[test]
fn host_reentry_makes_import_callers_cyclic_only_with_reentrant_imports() {
    let wat = r#"(module
        (import "env" "host" (func $host))
        (func $f (export "f") (call $host)))"#;
    let wasm = wat::parse_str(wat).expect("valid wat");
    let plain = wasm2rs::transpile_with_options(
        &wasm,
        &TranspileOptions::default().with_call_depth_limit(Some(100)),
    )
    .expect("transpile ok");
    assert_eq!(guard_sites(&plain), 0, "{plain}");
    // With re-entry the host may call `f` back from inside `host`: f -> host -> f.
    let reentrant = wasm2rs::transpile_with_options(
        &wasm,
        &TranspileOptions::default()
            .with_call_depth_limit(Some(100))
            .with_reentrant_imports(true),
    )
    .expect("transpile ok");
    assert_eq!(guard_sites(&reentrant), 1, "{reentrant}");
}

#[test]
fn continuations_fall_back_to_guarding_every_function() {
    let wat = r#"(module
        (elem declare func $gen)
        (type $ft (func (result i32)))
        (type $ct (cont $ft))
        (func $gen (result i32) i32.const 1)
        (func $leaf (result i32) i32.const 2)
        (func (export "run") (result i32)
          ref.func $gen cont.new $ct drop
          call $leaf))"#;
    // `$gen` is a step function (never guarded); the two ordinary functions are.
    assert_eq!(guard_sites(&transpile_limited(wat, 100)), 2);
}

#[test]
fn an_exception_unwinding_through_guarded_frames_restores_the_depth() {
    // `$rec` throws from 400 frames down; the guest catches it. Repeating that
    // five times at a limit of 500 would trap on the second round if the unwound
    // frames leaked their count.
    compile_run_limited(
        "depth_unwind_restore",
        r#"(module
            (tag $t)
            (func $rec (param i32)
              local.get 0
              if
                local.get 0 i32.const 1 i32.sub call $rec
              else
                throw $t
              end)
            (func $run (export "run") (result i32)
              (block $caught
                (try_table (catch_all $caught)
                  (call $rec (i32.const 400)))
                (return (i32.const 0)))
              i32.const 1))"#,
        500,
        "for _ in 0..5 {
            assert_eq!(func1(), 1);
        }",
    );
}
