//! Contract tests for the transpiler's *rejection* behaviour.
//!
//! `transpile` must reject wasm modules that parse cleanly but use a feature
//! wasm2rs does not support yet, and it must do so with a specific
//! `TranspileError::Unsupported` message rather than silently mistranspiling.
//! Those modules are valid wasm, so the error comes from wasm2rs' own checks; the
//! invalid ones (see `assert_invalid`) are instead rejected by `wasmparser`
//! validation.
//!
//! Each test asserts the error *category and message*, not merely `is_err()`, so
//! that a regression which starts accepting an unsupported feature — or reports
//! the wrong reason — fails here.

use wasm2rs::{TranspileError, transpile};

/// Assemble `wat`, transpile it, and assert it is rejected as `Unsupported`
/// with a message containing `needle`.
fn assert_unsupported(wat: &str, needle: &str) {
    let wasm = wat::parse_str(wat).expect("wat should assemble to valid wasm");
    match transpile(&wasm).as_ref().map_err(TranspileError::root) {
        Err(TranspileError::Unsupported(msg)) => assert!(
            msg.contains(needle),
            "wrong rejection reason: expected to contain {needle:?}, got {msg:?}",
        ),
        other => panic!("expected Unsupported({needle:?}), got {other:?}"),
    }
}

/// Assemble `wat` (which does not type-check) and assert `transpile` rejects it at
/// validation (`Parse`). Several of these modules once exercised wasm2rs' own
/// guards, which ill-typed input can no longer reach past `validate_all`.
fn assert_invalid(wat: &str) {
    let wasm = wat::parse_str(wat).expect("wat should assemble");
    let result = transpile(&wasm);
    assert!(
        matches!(result, Err(TranspileError::Parse(_))),
        "expected validation to reject the module, got {result:?}"
    );
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
fn a_mutable_imported_global_table_initializer_is_rejected() {
    // An immutable imported global is read from the host when the instance is
    // built (see imports.rs); a mutable one is not a constant.
    assert_invalid(
        r#"(module (import "e" "g" (global $g (mut funcref))) (table 1 1 funcref (global.get $g)))"#,
    );
}

#[test]
fn a_defined_global_seeded_from_an_imported_one_is_not_a_constant_item() {
    // Its value is only known to the host, so it has no transpile-time handle.
    assert_unsupported(
        r#"(module (import "e" "g" (global funcref))
            (global $d funcref (global.get 0))
            (table 1 funcref) (elem (i32.const 0) funcref (global.get $d)))"#,
        "element segment item reading a mutable, non-reference or non-constant global",
    );
}

#[test]
fn an_imported_global_item_in_a_passive_element_segment_is_rejected() {
    // A passive segment's items live in a static, which cannot hold the host's value.
    assert_unsupported(
        r#"(module (import "e" "g" (global $g funcref))
            (table 1 funcref) (elem funcref (global.get $g)))"#,
        "element segment item reading an imported global in a passive segment",
    );
}

#[test]
fn a_mutable_global_in_a_global_initializer_is_rejected() {
    // Only an immutable global is a constant: a mutable one's value can change, so
    // inlining its initializer would be wrong.
    assert_invalid(
        r#"(module
             (global $a (mut i32) (i32.const 1))
             (global $b i32 (global.get $a)))"#,
    );
}

#[test]
fn a_mutable_global_element_item_is_rejected() {
    assert_invalid(
        r#"(module
             (func $f)
             (global $g (mut funcref) (ref.func $f))
             (table 1 funcref)
             (elem (i32.const 0) funcref (global.get $g)))"#,
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
fn a_float_imported_global_offset_is_rejected() {
    assert_invalid(
        r#"(module (import "e" "g" (global f32)) (memory 1) (data (global.get 0) "x"))"#,
    );
}

#[test]
fn an_i64_imported_global_offset_of_a_32_bit_memory_is_rejected() {
    // The offset's width follows the memory's: an i64 one is not an i32 offset.
    assert_invalid(
        r#"(module (import "e" "g" (global i64)) (memory 1) (data (global.get 0) "x"))"#,
    );
}

#[test]
fn an_i32_imported_global_offset_of_a_64_bit_memory_is_rejected() {
    assert_invalid(
        r#"(module (import "e" "g" (global i32)) (memory i64 1) (data (global.get 0) "x"))"#,
    );
}

#[test]
fn a_32_bit_constant_in_a_64_bit_runtime_offset_is_rejected() {
    assert_invalid(
        r#"(module (import "e" "g" (global i64)) (memory i64 1)
             (data (i64.add (global.get 0) (i32.const 1)) "x"))"#,
    );
}

#[test]
fn a_64_bit_constant_in_a_32_bit_runtime_offset_is_rejected() {
    assert_invalid(
        r#"(module (import "e" "g" (global i32)) (memory 1)
             (data (i32.add (global.get 0) (i64.const 1)) "x"))"#,
    );
}

#[test]
fn a_mutable_imported_global_element_offset_is_rejected() {
    // An immutable imported global is read from the host at instantiation (see
    // imports.rs); a mutable one is not a constant.
    assert_invalid(
        r#"(module
             (import "e" "g" (global (mut i32)))
             (table 1 funcref) (func $f)
             (elem (global.get 0) $f))"#,
    );
}

#[test]
fn a_mutable_imported_global_in_a_global_initializer_is_rejected() {
    // An immutable imported global is read from the host when the instance is
    // built (see imports.rs); a mutable one is not a constant.
    assert_invalid(
        r#"(module
             (import "e" "g" (global (mut i32)))
             (global i32 (global.get 0)))"#,
    );
}

#[test]
fn memory_instruction_without_a_memory_section_is_rejected() {
    // A body that touches linear memory in a module that declares none must be
    // rejected rather than emitting code that references a non-existent field.
    assert_invalid(r#"(module (func (result i32) i32.const 0 i32.load))"#);
    // `memory.size` reaches the same guard through a different opcode.
    assert_invalid(r#"(module (func (result i32) memory.size))"#);
}

#[test]
fn table_instruction_without_a_table_section_is_rejected() {
    assert_invalid(r#"(module (func (result funcref) i32.const 0 table.get 0))"#);
}

#[test]
fn set_of_an_immutable_global_is_rejected() {
    // `global.set` on a non-`mut` global is invalid, so validation rejects it before
    // codegen could emit an assignment to an immutable field.
    assert_invalid(r#"(module (global i32 (i32.const 1)) (func i32.const 2 global.set 0))"#);
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
            (elem declare func $gen)
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
            (elem declare func $a $b)
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
              drop
              i32.const 0))"#,
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
            (elem declare func $a $b)
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
            (elem declare func $gen)
            (func $gen (result i32) i32.const 1)
            (func (export "run") (result i32)
              ref.func $gen cont.new $ct drop
              call $gen))"#,
        "called directly",
    );
}

#[test]
fn continuation_body_also_exported_is_rejected() {
    // An exported step function would need a `func{N}` wrapper that is never emitted.
    assert_unsupported(
        r#"(module
            (type $ft (func (result i32)))
            (type $ct (cont $ft))
            (elem declare func $gen)
            (func $gen (export "gen") (result i32) i32.const 1)
            (func (export "run") (result i32)
              ref.func $gen cont.new $ct drop
              i32.const 0))"#,
        "exported or used in a global initialiser",
    );
}

#[test]
fn continuation_body_in_a_global_initialiser_is_rejected() {
    assert_unsupported(
        r#"(module
            (type $ft (func (result i32)))
            (type $ct (cont $ft))
            (func $gen (result i32) i32.const 1)
            (global $g funcref (ref.func $gen))
            (func (export "run") (result i32)
              ref.func $gen cont.new $ct drop
              i32.const 0))"#,
        "exported or used in a global initialiser",
    );
}

#[test]
fn ref_test_on_a_function_type_is_rejected_when_the_host_may_hand_out_funcrefs() {
    // A funcref the host registered is an external handle with no signature to
    // compare, so a module with a host cannot answer the test.
    assert_unsupported(
        r#"(module
             (type $t (func (result i32)))
             (import "env" "tbl" (table 1 funcref))
             (func (result i32)
               (ref.test (ref $t) (table.get 0 (i32.const 0)))))"#,
        "external function",
    );
}

#[test]
fn call_indirect_without_a_table_is_rejected() {
    // `call_indirect` needs a table to resolve the callee. A module that uses it
    // with no table defined is invalid, so validation rejects it rather than
    // codegen emitting a call into a non-existent table. (The sibling "non-zero table index" guard is instead
    // shadowed by the earlier "multiple tables" rejection, so it is unreachable
    // from valid input and left untested by design.)
    assert_invalid(
        r#"(module
             (type $t (func))
             (func (call_indirect (type $t) (i32.const 0))))"#,
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
fn an_imported_shared_table_is_rejected() {
    assert_unsupported(
        r#"(module (import "env" "t" (table shared 1 1 (ref null (shared func)))))"#,
        "imported shared table",
    );
}

#[test]
fn a_shared_table_is_rejected() {
    assert_unsupported(
        r#"(module (table shared 1 1 (ref null (shared func))))"#,
        "shared table",
    );
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
    // validation rejects it rather than codegen emitting a bogus handle.
    assert_invalid(
        r#"(module
             (type $ft (func))
             (func (param (ref $ft))
               (cont.new $ft (local.get 0))
               (drop)))"#,
    );
}

#[test]
fn array_new_data_with_a_reference_element_is_rejected() {
    // `array.new_data` copies raw bytes from a data segment into an array. A
    // reference element type has no byte image to decode, so validation rejects it
    // rather than codegen fabricating references from data bytes.
    assert_invalid(
        r#"(module
             (type $a (array (mut funcref)))
             (data "\00\00\00\00")
             (func (result (ref $a))
               (array.new_data $a 0 (i32.const 0) (i32.const 1))))"#,
    );
}

#[test]
fn multiple_supertypes_are_rejected() {
    assert_invalid(
        r#"(module
            (rec
              (type $a (sub (struct)))
              (type $b (sub (struct)))
              (type $c (sub $a $b (struct))))
        )"#,
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
    // An exception stores its payload as raw `u64` bits. A `u32`-backed handle
    // (`funcref`, `externref`, `exnref`) fits, but a managed GC reference cannot,
    // so a tag with such a parameter must be refused rather than mistranspiled.
    assert_unsupported(
        r#"(module
            (tag $e (param anyref))
            (func (export "f")
              ref.null any
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
            (elem declare func $body $s $sp)
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
            (elem declare func $a)
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
fn return_call_from_a_continuation_body_with_a_suspend_inside_a_block_is_rejected() {
    // The sibling test above covers the plain lowering; here a `suspend` nested in a `block` makes
    // the body a pc state machine, which has its own `return_call` check.
    assert_unsupported(
        r#"(module
            (type $ft (func (result i32)))
            (type $ct (cont $ft))
            (tag $yield (param i32))
            (elem declare func $gen)
            (func $gen (result i32)
              (block
                i32.const 1 suspend $yield)
              return_call $inner)
            (func $inner (result i32)
              i32.const 7 suspend $yield
              i32.const 100)
            (func (export "run") (result i32)
              ref.func $gen cont.new $ct drop
              i32.const 0))"#,
        "return_call across a continuation",
    );
}

/// Invalid wasm (here a type-mismatched body) must be rejected up front instead of
/// being transpiled into silently wrong Rust.
#[test]
fn invalid_wasm_is_rejected_by_validation() {
    assert_invalid("(module (func i64.const 1 i64.const 2 i32.add drop))");
}

/// A function body that leaves an extra value on the stack is also invalid.
#[test]
fn leftover_stack_value_is_rejected_by_validation() {
    assert_invalid("(module (func i32.const 1))");
}

/// A rejection raised while transpiling a function body names the function and
/// keeps the underlying kind reachable through `root()`.
#[test]
fn a_function_body_rejection_is_wrapped_with_its_index() {
    let wasm = wat::parse_str(
        r#"(module
            (import "env" "f" (func))
            (tag $e (param v128))
            (func (export "f")
              v128.const i32x4 0 0 0 0
              throw $e))"#,
    )
    .expect("valid wat");
    let err = transpile(&wasm).expect_err("v128 payload is rejected");
    match &err {
        TranspileError::InFunction { index, source } => {
            assert_eq!(*index, 1, "{err}");
            assert!(matches!(**source, TranspileError::Unsupported(_)), "{err}");
        }
        other => panic!("expected InFunction, got {other:?}"),
    }
    assert!(
        err.to_string().starts_with("in function 1: unsupported: "),
        "{err}"
    );
    assert!(matches!(err.root(), TranspileError::Unsupported(_)));
}
