//! End-to-end tests for the legacy exception-handling proposal
//! (`try`/`catch`/`catch_all`/`throw`/`rethrow` and exception tags).

use crate::common;

use common::{compile_run, expect_trap};

#[test]
fn catch_returns_thrown_value() {
    // `throw $e` inside the `try` unwinds to the matching `catch $e`, which
    // pushes the exception's i32 payload — so the block yields the thrown value.
    compile_run(
        "eh_catch_value",
        r#"(module
            (tag $e (param i32))
            (func (export "f") (param i32) (result i32)
              try (result i32)
                local.get 0
                throw $e
              catch $e
              end))"#,
        "assert_eq!(func0(42), 42); assert_eq!(func0(-7), -7);",
    );
}

#[test]
fn catch_all_handles_any_tag() {
    // `catch_all` runs its handler for any thrown exception; the unreachable
    // `i32.const 99` after the throw is never the block's result.
    compile_run(
        "eh_catch_all",
        r#"(module
            (tag $e)
            (func (export "h") (result i32)
              try (result i32)
                throw $e
                i32.const 99
              catch_all
                i32.const 7
              end))"#,
        "assert_eq!(func0(), 7);",
    );
}

#[test]
fn throw_propagates_across_calls() {
    // The exception is thrown in a callee and caught in the caller's `try`,
    // exercising cross-function unwinding.
    compile_run(
        "eh_cross_call",
        r#"(module
            (tag $e (param i32))
            (func $thrower (param i32) (throw $e (local.get 0)))
            (func (export "f") (param i32) (result i32)
              try (result i32)
                local.get 0
                call $thrower
                i32.const 0
              catch $e
              end))"#,
        "assert_eq!(func1(5), 5);",
    );
}

#[test]
fn catch_selects_matching_tag_then_catch_all() {
    // A tagged `catch` only handles its own tag; an exception of another tag
    // falls through to `catch_all`. Two throwers pick which handler fires.
    compile_run(
        "eh_tag_select",
        r#"(module
            (tag $a (param i32))
            (tag $b (param i32))
            (func (export "f") (param i32) (result i32)
              try (result i32)
                local.get 0
                i32.const 0
                i32.eq
                if (result i32)
                  i32.const 100
                  throw $a
                else
                  i32.const 200
                  throw $b
                end
              catch $a
                ;; payload i32 is on the stack: return it + 1
                i32.const 1
                i32.add
              catch_all
                i32.const -1
              end))"#,
        "assert_eq!(func0(0), 101); assert_eq!(func0(9), -1);",
    );
}

#[test]
fn throw_carries_multiple_typed_values() {
    // A tag with several payload types round-trips each value through the
    // exception and back onto the stack in order (i32 below, i64 on top).
    compile_run(
        "eh_multi_value",
        r#"(module
            (tag $e (param i32 i64))
            (func (export "f") (result i64) (local i32 i64)
              try (result i64)
                i32.const 3
                i64.const 4
                throw $e
              catch $e
                ;; stack: i32 i64 (top i64); reorder via locals then add
                local.set 1
                local.set 0
                local.get 1
                local.get 0
                i64.extend_i32_s
                i64.add
              end))"#,
        "assert_eq!(func0(), 7);",
    );
}

#[test]
fn rethrow_reraises_to_outer_try() {
    // `rethrow` in a `catch_all` re-raises the caught exception; an outer `try`
    // then handles it.
    compile_run(
        "eh_rethrow",
        r#"(module
            (tag $e (param i32))
            (func (export "f") (param i32) (result i32)
              try (result i32)
                try (result i32)
                  local.get 0
                  throw $e
                catch_all
                  rethrow 0
                end
              catch $e
                i32.const 10
                i32.add
              end))"#,
        "assert_eq!(func0(5), 15);",
    );
}

#[test]
fn imported_tag_is_accepted_and_throwable() {
    // An imported tag occupies tag index 0; throwing and catching it works just
    // like a defined tag.
    compile_run(
        "eh_imported_tag",
        r#"(module
            (import "env" "e" (tag $e (param i32)))
            (func (export "f") (param i32) (result i32)
              try (result i32)
                local.get 0
                throw $e
              catch $e
              end))"#,
        "assert_eq!(func0(21), 21);",
    );
}

#[test]
fn branch_to_try_from_body_exits_normally() {
    // A `br` to the `try`'s own label from its body leaves the protected region
    // normally (no exception), yielding the branch's value.
    compile_run(
        "eh_br_to_try",
        r#"(module
            (tag $e)
            (func (export "f") (result i32)
              try (result i32)
                i32.const 5
                br 0
              catch_all
                i32.const 9
              end))"#,
        "assert_eq!(func0(), 5);",
    );
}

#[test]
fn non_falling_through_try_is_a_valid_tail() {
    // When the body and every handler diverge, the try never falls through;
    // it must still type-check as a value-returning function's tail.
    compile_run(
        "eh_diverging_try",
        r#"(module
            (tag $e)
            (func (export "f") (result i32)
              try (result i32)
                throw $e
              catch_all
                i32.const 7
                return
              end))"#,
        "assert_eq!(func0(), 7);",
    );
}

#[test]
fn br_escaping_try_body_to_outer_block() {
    // An unconditional `br` from inside the `try` body targets an *enclosing*
    // block, carrying the block's result value out of the `catch_unwind` closure.
    compile_run(
        "eh_br_escape_block",
        r#"(module
            (tag $e)
            (func (export "f") (result i32)
              (block (result i32)
                try
                  i32.const 5
                  br 1
                catch_all end
                i32.const 99)))"#,
        "assert_eq!(func0(), 5);",
    );
}

#[test]
fn br_if_escaping_try_body_is_conditional() {
    // A `br_if` from the `try` body escapes only when its condition holds; the
    // fall-through path stays inside the body and yields the block's own result.
    compile_run(
        "eh_br_if_escape",
        r#"(module
            (tag $e)
            (func (export "f") (param i32) (result i32)
              (block (result i32)
                try
                  i32.const 7
                  local.get 0
                  br_if 1
                  drop
                catch_all end
                i32.const 99)))"#,
        "assert_eq!(func0(1), 7); assert_eq!(func0(0), 99);",
    );
}

#[test]
fn return_escaping_try_body() {
    // A `return` inside the `try` body leaves both the closure and the function,
    // carrying the function's result value out.
    compile_run(
        "eh_return_escape",
        r#"(module
            (tag $e)
            (func (export "f") (param i32) (result i32)
              try
                local.get 0
                return
              catch_all end
              i32.const 42))"#,
        "assert_eq!(func0(3), 3); assert_eq!(func0(-1), -1);",
    );
}

#[test]
fn br_escaping_two_nested_try_bodies() {
    // A `br` from the innermost `try` body targets a block outside *both* trys,
    // so the escape signal must propagate out through each `catch_unwind` closure.
    compile_run(
        "eh_br_escape_nested",
        r#"(module
            (tag $e)
            (func (export "f") (result i32)
              (block (result i32)
                try
                  try
                    i32.const 8
                    br 2
                  catch_all end
                catch_all end
                i32.const 99)))"#,
        "assert_eq!(func0(), 8);",
    );
}

#[test]
fn br_escaping_try_body_to_outer_loop() {
    // A `br` to an enclosing loop from within a `try` body continues the loop
    // (its header), so a counter reaches its bound across repeated re-entries of
    // the try (and its per-iteration outcome variable).
    compile_run(
        "eh_br_escape_loop",
        r#"(module
            (tag $e)
            (func (export "f") (result i32)
              (local $i i32)
              (loop $l (result i32)
                try (result i32)
                  local.get $i
                  i32.const 3
                  i32.ge_s
                  if (result i32)
                    local.get $i          ;; done: yield the counter as the result
                  else
                    local.get $i
                    i32.const 1
                    i32.add
                    local.set $i
                    br 2                  ;; continue the loop $l
                  end
                catch_all
                  i32.const -1
                end)))"#,
        "assert_eq!(func0(), 3);",
    );
}

#[test]
fn br_from_catch_handler_to_outer_block() {
    // The shape LLVM's SjLj lowering relies on: a `br` from a *catch handler* to
    // an enclosing block. The handler runs in the landing pad, which sits inside
    // the outer block's labelled loop, so it lowers to a plain `break`.
    compile_run(
        "eh_br_from_handler",
        r#"(module
            (tag $e)
            (func (export "f") (result i32)
              (block (result i32)
                try (result i32)
                  throw $e
                catch_all
                  i32.const 7
                  br 1
                end
                drop
                i32.const 99)))"#,
        "assert_eq!(func0(), 7);",
    );
}

#[test]
fn throw_propagates_across_three_call_frames() {
    // A deeper unwind than `throw_propagates_across_calls`: the exception is
    // thrown at the bottom of a three-deep call chain ($a -> $b -> $c) and must
    // propagate up through every intermediate frame to the caller's `catch`.
    compile_run(
        "eh_cross_call_deep",
        r#"(module
            (tag $e (param i32))
            (func $c (param i32) (result i32) (local.get 0) (throw $e))
            (func $b (param i32) (result i32) (call $c (local.get 0)))
            (func $a (param i32) (result i32) (call $b (local.get 0)))
            (func (export "f") (param i32) (result i32)
              try (result i32)
                local.get 0
                call $a
              catch $e
              end))"#,
        // $c=func0, $b=func1, $a=func2, f=func3; stateless -> free functions.
        "assert_eq!(func3(42), 42); assert_eq!(func3(-5), -5);",
    );
}

#[test]
fn uncaught_throw_traps() {
    // An exception with no enclosing handler aborts the program.
    expect_trap(
        "eh_uncaught",
        r#"(module (tag $e) (func (export "g") (throw $e)))"#,
        "func0();",
    );
}

#[test]
fn multi_value_return_escaping_a_try_uses_the_tuple_holder() {
    // A `try` lowers its body into a closure, so a `return` inside it must escape
    // the closure and then perform the real function return. For a function with
    // two results that return goes through the *tuple* return-holder path
    // (`return (__rv0, __rv1)`), distinct from the single-result path. With a
    // non-zero argument the `return` fires with (11, 22); with zero the try
    // completes normally and the function yields (33, 44).
    compile_run(
        "eh_multi_return_through_try",
        r#"(module
            (func (export "f") (param i32) (result i32 i32)
              try (result i32 i32)
                local.get 0
                if (result i32 i32)
                  i32.const 11 i32.const 22
                  return
                else
                  i32.const 33 i32.const 44
                end
              catch_all
                i32.const 55 i32.const 66
              end))"#,
        "assert_eq!(func0(1), (11, 22)); assert_eq!(func0(0), (33, 44));",
    );
}

#[test]
fn float_payloads_keep_their_bit_patterns_through_throw_and_catch() {
    // The payload is stored bit-encoded in a `u64`. A quiet NaN with a nonzero
    // payload (f32) and -0.0 (f64) both change if the encoding goes through a
    // numeric conversion instead of `to_bits`/`from_bits`.
    compile_run(
        "eh_float_payload",
        r#"(module
            (tag $e (param f32 f64))
            (func (export "f") (result f32 f64)
              try (result f32 f64)
                f32.const nan:0x400001
                f64.const -0.0
                throw $e
              catch $e
              end))"#,
        "let (a, b) = func0(); \
         assert_eq!(a.to_bits(), 0x7fc0_0001); \
         assert_eq!(b.to_bits(), 0x8000_0000_0000_0000);",
    );
}

#[test]
fn delegate_forwards_exception_to_outer_handler() {
    // `delegate 0` has no handlers of its own: an exception thrown in its body
    // continues to the enclosing `try`, whose `catch $e` handles it.
    compile_run(
        "eh_delegate_outer",
        r#"(module
            (tag $e (param i32))
            (func (export "f") (param i32) (result i32)
              try (result i32)
                try (result i32)
                  local.get 0
                  throw $e
                delegate 0
              catch $e
                i32.const 10
                i32.add
              end))"#,
        "assert_eq!(func0(5), 15);",
    );
}

#[test]
fn delegate_body_falls_through_when_nothing_is_thrown() {
    compile_run(
        "eh_delegate_fallthrough",
        r#"(module
            (tag $e (param i32))
            (func (export "f") (param i32) (result i32)
              try (result i32)
                try (result i32)
                  local.get 0
                  i32.eqz
                  if
                    i32.const 1
                    throw $e
                  end
                  i32.const 20
                delegate 0
              catch $e
                i32.const 100
                i32.add
              end))"#,
        "assert_eq!(func0(1), 20); assert_eq!(func0(0), 101);",
    );
}

#[test]
fn delegate_to_caller_propagates_out_of_the_function() {
    // At function level, depth 0 names the function itself: the exception
    // leaves the function uncaught.
    expect_trap(
        "eh_delegate_caller",
        r#"(module
            (tag $e)
            (func (export "f") (result i32)
              try (result i32)
                throw $e
              delegate 0))"#,
        "func0();",
    );
}

#[test]
fn delegate_inside_catch_handler_skips_that_try() {
    // The shape clang emits for a cleanup inside a catch: the `delegate` sits in
    // the handler of the middle `try`, so it is outside that try's closure and
    // the exception goes straight to the outer `try`, not back to `catch_all`.
    compile_run(
        "eh_delegate_in_handler",
        r#"(module
            (tag $e (param i32))
            (func (export "f") (param i32) (result i32)
              try (result i32)
                try (result i32)
                  local.get 0
                  throw $e
                catch_all
                  try
                    local.get 0
                    i32.const 1
                    i32.add
                    throw $e
                  delegate 1
                  i32.const -1
                end
              catch $e
              end))"#,
        "assert_eq!(func0(5), 6);",
    );
}

#[test]
fn delegate_bypasses_an_enclosing_try_body() {
    // `delegate 1` skips the middle `try` (whose `catch $e` would answer 100) and
    // lands in the outer one.
    compile_run(
        "eh_delegate_bypass",
        r#"(module
            (tag $e)
            (func (export "f") (result i32)
              try (result i32)
                try (result i32)
                  try (result i32)
                    throw $e
                  delegate 1
                catch $e
                  i32.const 100
                end
              catch $e
                i32.const 7
              end))"#,
        "assert_eq!(func0(), 7);",
    );
}

#[test]
fn delegate_to_caller_bypasses_the_functions_own_handlers() {
    // The clang shape: inside a `try` that would `catch_all`, a nested
    // `try ... delegate 1` targets the caller. The callee's own `catch_all`
    // must not see it; the caller's handler does.
    compile_run(
        "eh_delegate_caller_bypass",
        r#"(module
            (tag $e)
            (func $inner (result i32)
              try (result i32)
                try (result i32)
                  throw $e
                delegate 1
              catch_all
                i32.const 99
              end)
            (func (export "f") (result i32)
              try (result i32)
                call $inner
              catch $e
                i32.const 7
              end))"#,
        "assert_eq!(func1(), 7);",
    );
}

#[test]
fn delegate_bypassing_several_bodies_counts_each_one() {
    // Two enclosing bodies are bypassed; both catch_all handlers are skipped and
    // the exception reaches the outermost handler.
    compile_run(
        "eh_delegate_two_bodies",
        r#"(module
            (tag $e)
            (func (export "f") (result i32)
              try (result i32)
                try (result i32)
                  try (result i32)
                    try (result i32)
                      throw $e
                    delegate 2
                  catch_all
                    i32.const 1
                  end
                catch_all
                  i32.const 2
                end
              catch $e
                i32.const 3
              end))"#,
        "assert_eq!(func0(), 3);",
    );
}

#[test]
fn caught_throw_does_not_run_the_panic_hook() {
    // A thrown-and-caught exception is control flow, not a failure: it must not
    // reach the default panic hook, which would print to stderr on every throw.
    // Covers `throw` (f) and `throw_ref` (g).
    let wat = r#"(module
        (tag $e (param i32))
        (func (export "f") (param i32) (result i32)
          try (result i32)
            local.get 0
            throw $e
          catch $e
          end)
        (func (export "g") (param i32) (result i32)
          (local $x exnref)
          (block $h (result exnref)
            (try_table (catch_all_ref $h) (throw $e (local.get 0)))
            (unreachable))
          local.set $x
          (block $o (result i32)
            (try_table (result i32) (catch $e $o)
              (throw_ref (local.get $x))))))"#;
    let wasm = wat::parse_str(wat).unwrap();
    let generated = wasm2rs::transpile(&wasm).unwrap();
    let program = format!(
        "{generated}\nfn main() {{ for i in 0..3 {{ assert_eq!(func0(i), i); assert_eq!(func1(i), i); }} }}\n"
    );
    let bin = common::build("eh_no_panic_hook", &program);
    let run = std::process::Command::new(&bin).output().unwrap();
    assert!(run.status.success());
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        !stderr.contains("panicked at"),
        "throw ran the panic hook:\n{stderr}"
    );
}
