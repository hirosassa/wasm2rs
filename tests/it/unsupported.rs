//! Contract tests for the transpiler's *rejection* behaviour.
//!
//! `transpile` must reject wasm modules that parse cleanly but use a feature
//! wasm2rs does not support yet, and it must do so with a specific
//! `TranspileError::Unsupported` message rather than silently mistranspiling.
//! These modules are all valid wasm (the `wat` crate assembles them); the error
//! therefore comes from wasm2rs' own validation, not from `wasmparser`.
//!
//! Each test asserts the error *category and message*, not merely `is_err()`, so
//! that a regression which starts accepting an unsupported feature — or reports
//! the wrong reason — fails here.

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

use wasm2rs::{TranspileError, transpile};

/// Assemble `wat`, transpile it, and assert it is rejected as `Unsupported`
/// with a message containing `needle`.
fn assert_unsupported(wat: &str, needle: &str) {
    let wasm = wat::parse_str(wat).expect("wat should assemble to valid wasm");
    match transpile(&wasm) {
        Err(TranspileError::Unsupported(msg)) => assert!(
            msg.contains(needle),
            "wrong rejection reason: expected to contain {needle:?}, got {msg:?}",
        ),
        other => panic!("expected Unsupported({needle:?}), got {other:?}"),
    }
}

#[test]
fn imported_non_zero_memory_is_rejected() {
    // Multiple *defined* memories are now supported (see tests/multi_memory.rs),
    // but a host-lent buffer is only wired up for memory 0, so an *imported*
    // memory at a higher index remains unsupported.
    assert_unsupported(
        r#"(module (import "e" "m0" (memory 1)) (import "e" "m1" (memory 1)))"#,
        "imported non-zero memory",
    );
}

#[test]
fn shared_memory64_is_rejected() {
    // Shared memory is backed by a `u32`-addressed `SharedMemory`, so a shared
    // 64-bit memory is not supported (a plain 64-bit memory is; see memory64.rs).
    assert_unsupported(
        r#"(module (memory i64 1 1 shared))"#,
        "shared 64-bit memory",
    );
}

#[test]
fn an_imported_global_table_initializer_is_rejected() {
    // `ref.func`, `ref.null` and `global.get` of a defined global are supported (see
    // elem_exprs.rs); reading an imported global is not.
    assert_unsupported(
        r#"(module (import "e" "g" (global $g funcref)) (table 1 1 funcref (global.get $g)))"#,
        "table initializer reading an imported, mutable or non-reference global",
    );
}

#[test]
fn an_imported_global_element_item_is_rejected() {
    assert_unsupported(
        r#"(module (import "e" "g" (global $g funcref))
            (table 1 funcref) (elem (i32.const 0) funcref (global.get $g)))"#,
        "element segment item reading an imported, mutable or non-reference global",
    );
}

#[test]
fn a_mutable_global_in_a_global_initializer_is_rejected() {
    // Only an immutable global is a constant: a mutable one's value can change, so
    // inlining its initializer would be wrong.
    assert_unsupported(
        r#"(module
             (global $a (mut i32) (i32.const 1))
             (global $b i32 (global.get $a)))"#,
        "imported, mutable or later global",
    );
}

#[test]
fn a_mutable_global_element_item_is_rejected() {
    assert_unsupported(
        r#"(module
             (func $f)
             (global $g (mut funcref) (ref.func $f))
             (table 1 funcref)
             (elem (i32.const 0) funcref (global.get $g)))"#,
        "element segment item reading an imported, mutable or non-reference global",
    );
}

#[test]
fn a_try_table_catch_all_ref_with_an_exnref_result_is_rejected() {
    // `catch $tag $l` and `catch_all $l` are lowered (see try_table.rs); the
    // `_ref` forms capture the exception as an `exnref`, which has no lowering yet
    // (the `exnref` value type is rejected before the clause itself is reached).
    assert_unsupported(
        r#"(module
            (tag $e)
            (func (result exnref)
              (block $b (result exnref)
                try_table (result exnref) (catch_all_ref $b)
                  ref.null exn
                end)))"#,
        "exnref",
    );
}

#[test]
fn br_table_out_of_try_body_is_rejected() {
    // A `br` and `br_if` escaping a `try` body are lowered via a closure-outcome
    // signal, but a `br_table` whose arms leave the body (mixing escaping and
    // non-escaping targets) is not lowered yet.
    assert_unsupported(
        r#"(module
            (tag $e)
            (func (param i32)
              (block $out
                try
                  local.get 0
                  br_table $out $out
                catch_all
                end)))"#,
        "br_table out of a try region",
    );
}

#[test]
fn branch_to_try_from_handler_is_rejected() {
    // A `br` to the try's own label from a catch handler would break out of the
    // landing-pad `match`, which is not a loop.
    assert_unsupported(
        r#"(module
            (tag $e)
            (func
              try
                throw $e
              catch_all
                br 0
              end))"#,
        "branch out of a try handler",
    );
}

#[test]
fn multi_value_import_result_is_rejected() {
    // Defined functions may return multiple values, but an imported function
    // that returns more than one value is not supported yet.
    assert_unsupported(
        r#"(module (import "e" "f" (func (result i32 i32))))"#,
        "multi-value import result",
    );
}

#[test]
fn throw_ref_is_rejected_with_the_operator_named() {
    // A not-yet-implemented operator (here `throw_ref`, the exnref half of the
    // exception-handling proposal) is rejected with the operator named, so the gap
    // is diagnosable rather than a generic failure.
    assert_unsupported(
        r#"(module
            (func (param exnref)
              (throw_ref (local.get 0))))"#,
        "ThrowRef",
    );
}

#[test]
fn non_constant_element_offset_is_rejected() {
    // The element offset must be a plain `i32.const`; a `global.get` offset
    // (permitted by wasm) is not supported.
    assert_unsupported(
        r#"(module
             (import "e" "g" (global i32))
             (table 1 funcref) (func $f)
             (elem (global.get 0) $f))"#,
        "element offset",
    );
}

#[test]
fn non_constant_global_initializer_is_rejected() {
    // Reading an *imported* global in a constant expression needs the host at
    // construction time and is not supported (a defined one is; see elem_exprs.rs).
    assert_unsupported(
        r#"(module
             (import "e" "g" (global i32))
             (global i32 (global.get 0)))"#,
        "imported, mutable or later global",
    );
}

#[test]
fn memory_instruction_without_a_memory_section_is_rejected() {
    // A body that touches linear memory in a module that declares none must be
    // rejected rather than emitting code that references a non-existent field.
    assert_unsupported(
        r#"(module (func (result i32) i32.const 0 i32.load))"#,
        "memory instruction without a memory section",
    );
    // `memory.size` reaches the same guard through a different opcode.
    assert_unsupported(
        r#"(module (func (result i32) memory.size))"#,
        "memory instruction without a memory section",
    );
}

#[test]
fn table_instruction_without_a_table_section_is_rejected() {
    assert_unsupported(
        r#"(module (func (result funcref) i32.const 0 table.get 0))"#,
        "table instruction without a table section",
    );
}

#[test]
fn set_of_an_immutable_global_is_rejected() {
    // `global.set` on a non-`mut` global is invalid; wasm2rs rejects it with a
    // specific reason rather than generating an assignment to an immutable field.
    assert_unsupported(
        r#"(module (global i32 (i32.const 1)) (func i32.const 2 global.set 0))"#,
        "set of immutable global",
    );
}

#[test]
fn suspend_outside_a_continuation_is_rejected() {
    // `suspend`/`resume` are now implemented for continuations (see
    // tests/cont.rs), including propagation across calls. But `suspend` only
    // makes sense inside a function reachable as a continuation body: a bare
    // `suspend` in a function that is never turned into a continuation would
    // suspend with no handler, so it is rejected rather than mistranslated.
    assert_unsupported(
        r#"(module
            (type $ft (func))
            (tag $t)
            (func $f suspend $t))"#,
        "not reachable as a continuation",
    );
}

#[test]
fn resume_throw_with_suspend_handlers_is_rejected() {
    // `resume_throw` is implemented (see tests/cont.rs), but only for the case
    // where the injected exception propagates straight out of the continuation:
    // a continuation body cannot install an exception handler, so its suspend
    // handlers could only fire after an internal catch that never happens. A
    // resume_throw carrying an `(on $tag $label)` handler is therefore rejected
    // rather than mistranslated into an unreachable dispatch.
    assert_unsupported(
        r#"(module
            (type $ft (func (result i32)))
            (type $ct (cont $ft))
            (tag $yield (param i32))
            (tag $cancel (param i32))
            (func $gen (result i32)
              i32.const 1 suspend $yield
              i32.const 2)
            (func (export "run") (result i32)
              (local $k (ref null $ct))
              ref.func $gen cont.new $ct local.set $k
              (block $on_yield (result i32 (ref $ct))
                i32.const 0
                local.get $k
                resume_throw $ct $cancel (on $yield $on_yield)
                return)
              drop))"#,
        "resume_throw with suspend handlers",
    );
}

#[test]
fn resume_mixing_switch_and_label_handlers_is_rejected() {
    // A single `resume` can carry an `(on $tag switch)` handler (the switch
    // trampoline, see tests/cont.rs) or `(on $tag $label)` suspend handlers, but
    // not both at once: the trampoline follows switches through a chain of
    // continuations, so handing a suspending continuation back to a label would
    // have to use the *currently driven* handle rather than the originally
    // resumed one. That re-dispatch is not lowered yet (phase 8), so the mix is
    // rejected rather than mistranslated into handing back the wrong handle.
    assert_unsupported(
        r#"(module
            (type $ht (func (result i32)))
            (tag $t (type $ht))
            (tag $y (param i32))
            (type $fa (func (result i32)))
            (type $ca (cont $fa))
            (type $fa_s (func (result i32)))
            (type $ca_s (cont $fa_s))
            (type $fb (func (param i32) (param (ref $ca_s)) (result i32)))
            (type $cb (cont $fb))
            (func $a (type $fa)
              i32.const 11
              ref.func $b cont.new $cb
              switch $cb $t
              i32.const 999)
            (func $b (type $fb)
              local.get 0 i32.const 2 i32.add)
            (func (export "run") (type $fa)
              (local $k (ref null $ca))
              (block $on_yield (result i32 (ref $ca))
                ref.func $a cont.new $ca
                resume $ca (on $y $on_yield) (on $t switch)
                return)
              local.set $k
              drop))"#,
        "combining a switch handler with a suspend-to-label handler",
    );
}

#[test]
fn entry_checkpoint_in_a_parameterized_continuation_is_rejected() {
    // A cross-call checkpoint sitting at the very entry (`pc == 0`) of a
    // continuation body that has parameters (P8 follow-up). The header decodes the
    // body's parameters from `__args` whenever `pc == 0`, and an entry checkpoint
    // never advances `pc` past 0, so a switch-back — which re-enters the step with
    // the injection in `__args` — would re-run that prologue and clobber the
    // body's own parameter slots with the injected values. Reject the combination
    // rather than miscompile; a parameter-less body (or one whose checkpoint sits
    // past `pc == 0` because it suspends first) is unaffected.
    assert_unsupported(
        r#"(module
            (type $ht (func (result i32)))
            (tag $t (type $ht))
            (type $fa (func (param i32) (result i32)))
            (type $ca (cont $fa))
            (type $fc (func (result i32)))
            (type $fc_s (func (result i32)))
            (type $cc_s (cont $fc_s))
            (type $fb (func (param i32) (param (ref $cc_s)) (result i32)))
            (type $cb (cont $fb))
            (func $a (type $fa)
              call $c)
            (func $b (type $fb)
              local.get 0 i32.const 2 i32.add)
            (func $c (type $fc)
              i32.const 11
              ref.func $b cont.new $cb
              switch $cb $t
              i32.const 888)
            (func (export "run") (result i32)
              i32.const 5
              ref.func $a cont.new $ca
              resume $ca (on $t switch)))"#,
        "cross-call checkpoint at the entry of a continuation body with parameters",
    );
}

#[test]
fn continuation_body_also_called_directly_is_rejected() {
    // A function used as a continuation body is emitted only as a resumable
    // step function, never as an ordinary `func{N}`, so also calling it
    // directly would reference a method that does not exist. Reject it cleanly
    // rather than emitting code that fails to compile.
    assert_unsupported(
        r#"(module
            (type $ft (func (result i32)))
            (type $ct (cont $ft))
            (func $gen (result i32) i32.const 1)
            (func (export "run") (result i32)
              ref.func $gen cont.new $ct drop
              call $gen))"#,
        "called directly",
    );
}

#[test]
fn ref_null_of_a_non_reference_heaptype_is_rejected() {
    // `funcref`/`externref` nulls lower to the `u32::MAX` sentinel and the
    // abstract GC heaptypes (`any`/`eq`/`struct`/`array`/`none`) lower to the
    // managed `GcRef::Null`. An `exn` (exception) reference has neither
    // representation, so it is rejected by name rather than mistranslated.
    assert_unsupported(
        r#"(module (func (result i32) (ref.is_null (ref.null exn))))"#,
        "ref.null of unsupported type",
    );
}

#[test]
fn call_indirect_without_a_table_is_rejected() {
    // `call_indirect` needs a table to resolve the callee. A module that uses it
    // with no table defined still assembles as valid wasm, but wasm2rs has
    // nothing to dispatch through, so it rejects rather than emitting a call into
    // a non-existent table. (The sibling "non-zero table index" guard is instead
    // shadowed by the earlier "multiple tables" rejection, so it is unreachable
    // from valid input and left untested by design.)
    assert_unsupported(
        r#"(module
             (type $t (func))
             (func (call_indirect (type $t) (i32.const 0))))"#,
        "call_indirect without a table",
    );
}

#[test]
fn a_table_of_a_non_funcref_element_type_is_rejected() {
    // A `funcref`/`externref` table is wired to the dispatch machinery, but the
    // GC proposal permits a table of any reference type. wasm2rs only supports
    // the two function/host element types, so a table of a concrete struct
    // reference is valid wasm that wasm2rs declines rather than mistranslates.
    assert_unsupported(
        r#"(module
             (type $s (struct))
             (table 1 (ref null $s)))"#,
        "table of a non-funcref/externref type",
    );
}

#[test]
fn an_imported_sixty_four_bit_table_is_rejected() {
    // A defined table64 is supported; an imported one is host-lent storage
    // addressed through the `Imports` trait with `u32` indices.
    assert_unsupported(
        r#"(module (import "env" "t" (table i64 1 funcref)))"#,
        "imported 64-bit or shared table",
    );
}

#[test]
fn a_shared_table_is_rejected() {
    assert_unsupported(r#"(module (table shared 1 1 funcref))"#, "shared table");
}

#[test]
fn an_imported_shared_64_bit_memory_is_rejected() {
    // An imported shared memory is supported (a host-created `SharedMemory`
    // passed to `Instance::new`), but like a defined one it is addressed by
    // `u32`, so a 64-bit shared memory is rejected.
    assert_unsupported(
        r#"(module (import "e" "m" (memory i64 1 1 shared)))"#,
        "shared 64-bit memory",
    );
}

#[test]
fn cont_new_on_a_non_continuation_type_is_rejected() {
    // `cont.new $t` reifies a function reference into a continuation. When `$t`
    // names a non-continuation composite type the operator is meaningless;
    // wasm2rs rejects it by index rather than emitting a bogus handle.
    assert_unsupported(
        r#"(module
             (type $ft (func))
             (func (param (ref $ft))
               (cont.new $ft (local.get 0))
               (drop)))"#,
        "is not a continuation type",
    );
}

#[test]
fn array_new_data_with_a_reference_element_is_rejected() {
    // `array.new_data` copies raw bytes from a data segment into an array. A
    // reference element type has no byte image to decode, so wasm2rs rejects it
    // rather than fabricating references from data bytes.
    assert_unsupported(
        r#"(module
             (type $a (array (mut funcref)))
             (data "\00\00\00\00")
             (func (result (ref $a))
               (array.new_data $a 0 (i32.const 0) (i32.const 1))))"#,
        "array.new_data/init_data with a reference element type",
    );
}

#[test]
fn ref_test_on_a_func_heaptype_is_rejected() {
    // `ref.test` in wasm2rs is implemented for the managed GC hierarchy
    // (`GcRef`). A test against the `func` heap type operates on the funcref
    // hierarchy, which wasm2rs represents as opaque `u32` handles with no
    // runtime type tag, so the operator is rejected rather than mistranslated.
    assert_unsupported(
        r#"(module
             (func (param funcref) (result i32)
               (ref.test (ref func) (local.get 0))))"#,
        "func/extern heap type",
    );
}

#[test]
fn multiple_supertypes_are_rejected() {
    assert_unsupported(
        r#"(module
            (rec
              (type $a (sub (struct)))
              (type $b (sub (struct)))
              (type $c (sub $a $b (struct))))
        )"#,
        "multiple supertypes",
    );
}

#[test]
fn memory_access_targeting_a_gc_array_is_rejected() {
    assert_unsupported(
        r#"(module
            (memory 1)
            (type $arr (array (mut i8)))
            (func (param (ref $arr)) (result i32)
              local.get 0
              i32.const 0
              i32.load (type $arr)))"#,
        "GC array payload",
    );
}

#[test]
fn exception_payload_with_a_reference_type_is_rejected() {
    // An exception stores its payload as raw `u64` bits, which cannot hold a
    // managed reference, so a tag with a reference-typed parameter must be
    // refused rather than mistranspiled.
    assert_unsupported(
        r#"(module
            (tag $e (param funcref))
            (func (export "f")
              ref.null func
              throw $e))"#,
        "exception payload with a reference type",
    );
}

#[test]
fn exception_payload_with_a_v128_is_rejected() {
    assert_unsupported(
        r#"(module
            (tag $e (param v128))
            (func (export "f")
              v128.const i32x4 0 0 0 0
              throw $e))"#,
        "exception payload with a v128 value",
    );
}

/// A module whose continuation body `$body` is `body`, next to `$s` (a
/// suspending step function, so a call to it from `$body` is a cross-call
/// checkpoint) and `$sp` (the same but taking one i32 parameter).
fn cont_body_wat(body: &str) -> String {
    format!(
        r#"(module
            (type $ft (func (result i32)))
            (type $ct (cont $ft))
            (type $fp (func (param i32) (result i32)))
            (type $cp (cont $fp))
            (tag $y (param i32))
            (func $s (result i32)
              i32.const 1 suspend $y
              i32.const 2)
            (func $sp (param i32) (result i32)
              i32.const 1 suspend $y
              i32.const 2)
            (func $body (result i32) {body})
            (func (export "run") (result i32)
              (block $on (result i32 (ref $ct))
                ref.func $body cont.new $ct
                resume $ct (on $y $on)
                return)
              drop drop
              ;; make `$s`/`$sp` reachable as continuation bodies in their own right
              ref.func $s cont.new $ct drop
              ref.func $sp cont.new $cp drop
              i32.const 0))"#
    )
}

#[test]
fn two_cross_call_checkpoints_in_one_body_are_rejected() {
    // A body is lowered with at most one checkpoint (its tail call); a second
    // one would need a second saved callee frame.
    assert_unsupported(
        &cont_body_wat("call $s drop call $s"),
        "more than one cross-call checkpoint",
    );
}

#[test]
fn checkpoint_with_a_live_operand_is_rejected() {
    // `10` is still on the operand stack when `$s` is called; it would have to
    // survive the callee's suspension, which the checkpoint cannot save.
    assert_unsupported(
        &cont_body_wat("i32.const 10 call $s i32.add"),
        "non-empty operand stack before a cross-call checkpoint",
    );
}

#[test]
fn checkpoint_to_a_function_with_parameters_is_rejected() {
    assert_unsupported(
        &cont_body_wat("i32.const 3 call $sp"),
        "cross-call checkpoint to a continuation with parameters",
    );
}

#[test]
fn checkpoint_inside_a_block_is_rejected() {
    assert_unsupported(
        &cont_body_wat("block (result i32) call $s end"),
        "cross-call checkpoint inside nested control flow",
    );
}

#[test]
fn return_call_into_a_suspending_function_is_rejected() {
    assert_unsupported(
        &cont_body_wat("return_call $s"),
        "return_call across a continuation",
    );
}

#[test]
fn a_second_imported_table_is_rejected() {
    // Only one table is supported; the limit applies to imports too.
    assert_unsupported(
        r#"(module (import "e" "t0" (table 1 funcref)) (import "e" "t1" (table 1 funcref)))"#,
        "multiple tables",
    );
}

#[test]
fn imported_table_of_a_gc_reference_type_is_rejected() {
    assert_unsupported(
        r#"(module (import "e" "t" (table 1 anyref)))"#,
        "table of a non-funcref/externref type",
    );
}

#[test]
fn error_display_names_the_failure_kind() {
    // The CLI prints `TranspileError` via `Display`, so the prefix is how a
    // user tells a malformed input from a merely unsupported one.
    let parse = transpile(b"definitely not wasm").unwrap_err();
    assert!(matches!(parse, TranspileError::Parse(_)));
    assert!(
        parse.to_string().starts_with("failed to parse wasm: "),
        "got {parse}",
    );

    let wasm =
        wat::parse_str(r#"(module (import "e" "m0" (memory 1)) (import "e" "m1" (memory 1)))"#)
            .unwrap();
    let unsupported = transpile(&wasm).unwrap_err();
    assert_eq!(
        unsupported.to_string(),
        "unsupported: imported non-zero memory",
    );

    assert_eq!(
        TranspileError::StackUnderflow.to_string(),
        "operand stack underflow",
    );
}

#[test]
fn checkpoint_in_a_body_that_branches_to_its_function_label_is_rejected() {
    // A body that branches to its own label is wrapped in a synthetic block, so a
    // cross-call checkpoint in it is no longer at region depth 0. Fail loudly.
    assert_unsupported(
        r#"(module
            (type $ft (func (result i32)))
            (tag $y (param i32))
            (func $c (type $ft) i32.const 1 suspend $y i32.const 2)
            (type $ct (cont $ft))
            (func $a (type $ft) call $c br 0)
            (func (export "run") (result i32)
              ref.func $a cont.new $ct
              drop
              i32.const 0))"#,
        "cross-call checkpoint inside nested control flow",
    );
}

#[test]
fn a_try_table_clause_branching_to_a_try_whose_handler_it_is_in_is_rejected() {
    // The landing pad of `$t` lies outside its body loop, so a branch to `$t` from
    // inside its own handler has no lowering, even through a nested region.
    assert_unsupported(
        r#"(module
            (tag $e) (tag $f)
            (func (result i32)
              try $t
                throw $e
              catch $e
                try_table (catch $f $t)
                  throw $f
                end
              end
              i32.const 1))"#,
        "branch out of a try handler",
    );
}

#[test]
fn a_try_table_catch_ref_clause_is_rejected_by_name() {
    // The transpiler does not validate, so a `catch_ref` / `catch_all_ref` clause
    // is reached (and rejected) even when its label has no `exnref` type.
    for clause in ["(catch_ref $e $b)", "(catch_all_ref $b)"] {
        assert_unsupported(
            &format!(
                r#"(module
                    (tag $e)
                    (func (block $b (try_table {clause}))))"#
            ),
            "catch_ref",
        );
    }
}
