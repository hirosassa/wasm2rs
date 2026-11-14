//! Tests for the opt-in `TranspileOptions::typed_traps` (M6 stage 1).
//!
//! With the option on, the five runtime-helper traps (`unreachable`, indirect call
//! type mismatch, call stack exhausted and the two float->int conversion traps)
//! unwind with a `Wasm2RsTrap` payload (`resume_unwind`, so the panic hook never
//! runs) instead of a message string, the same mechanism a wasm exception uses.
//! Stage 2 adds integer division/remainder and linear-memory / table out-of-bounds
//! traps (explicit checks in place of Rust's `/` and slice indexing). Stage 3a adds
//! the shared-memory atomics (a misaligned or out-of-bounds atomic access) and the
//! plain accesses of a shared memory. The remaining traps (GC, continuations,
//! `exnref`, glue) keep their string payload, and the option is off by default.

use std::process::Command;
use wasm2rs::TranspileOptions;

use crate::common;

/// Functions 1-5 trap in the five helper ways; 6-10 call them under a `catch_all`,
/// which must not catch a trap; 11 is a division by zero (a stage-2 trap).
const TRAPS: &str = r#"(module
    (type $void (func))
    (tag $e)
    (table funcref (elem $ret))
    (func $ret (result i32) i32.const 1)
    (func $unr (export "unr") unreachable)
    (func $mism (export "mism") (call_indirect (type $void) (i32.const 0)))
    (func $runaway (export "runaway") (call $runaway))
    (func $nan (export "nan") (param f32) (result i32) (i32.trunc_f32_s (local.get 0)))
    (func $ovf (export "ovf") (param f64) (result i32) (i32.trunc_f64_s (local.get 0)))
    (func (export "c_unr") try call $unr catch_all end)
    (func (export "c_mism") try call $mism catch_all end)
    (func (export "c_runaway") try call $runaway catch_all end)
    (func (export "c_nan") (result i32)
      try (result i32) f32.const nan call $nan catch_all i32.const 0 end)
    (func (export "c_ovf") (result i32)
      try (result i32) f64.const 1e30 call $ovf catch_all i32.const 0 end)
    (func (export "div") (param i32) (result i32) (i32.div_s (i32.const 1) (local.get 0))))"#;

fn transpile_typed(wat: &str, typed: bool) -> String {
    transpile_with(wat, TranspileOptions::default().with_typed_traps(typed))
}

fn transpile_with(wat: &str, opts: TranspileOptions) -> String {
    let wasm = wat::parse_str(wat).expect("valid wat");
    wasm2rs::transpile_with_options(&wasm, &opts.with_call_depth_limit(Some(100)))
        .expect("transpile ok")
}

fn compile_run_typed(name: &str, wat: &str, main_body: &str) {
    compile_run_with(
        name,
        wat,
        TranspileOptions::default().with_typed_traps(true),
        main_body,
    );
}

fn compile_run_with(name: &str, wat: &str, opts: TranspileOptions, main_body: &str) {
    let program = format!(
        "{}\nfn main() {{\n{main_body}\n}}\n",
        transpile_with(wat, opts)
    );
    let bin = common::build(name, &program);
    let run = Command::new(&bin).status().expect("run generated binary");
    assert!(
        run.success(),
        "generated program assertions failed:\n{name}"
    );
}

/// Runs `f`, returning its panic payload; the hook records whether it ran.
const CATCH: &str = r#"
#[allow(dead_code)]
static HOOK_RAN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
#[allow(dead_code)]
fn payload_of<R>(f: impl FnOnce() -> R) -> Box<dyn std::any::Any + Send> {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| HOOK_RAN.store(true, std::sync::atomic::Ordering::SeqCst)));
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    std::panic::set_hook(prev);
    match r {
        Err(p) => p,
        Ok(_) => panic!("expected a trap"),
    }
}
#[allow(dead_code)]
fn trap_of<R>(f: impl FnOnce() -> R) -> Wasm2RsTrap {
    *payload_of(f).downcast::<Wasm2RsTrap>().expect("a Wasm2RsTrap payload")
}
#[allow(dead_code)]
fn assert_send_static<T: Send + 'static + std::error::Error>() {}
"#;

#[test]
fn off_by_default_keeps_string_traps_and_emits_no_trap_type() {
    let wasm = wat::parse_str(TRAPS).expect("valid wat");
    let plain = wasm2rs::transpile_with_options(
        &wasm,
        &TranspileOptions::default().with_call_depth_limit(Some(100)),
    )
    .expect("transpile ok");
    assert!(!plain.contains("Wasm2RsTrap"), "{plain}");
    assert_eq!(plain, transpile_typed(TRAPS, false));
    assert!(transpile_typed(TRAPS, true).contains("Wasm2RsTrap"));
}

#[test]
fn only_the_used_variants_are_emitted() {
    let wasm = wat::parse_str(r#"(module (func (export "u") unreachable))"#).expect("valid wat");
    let out =
        wasm2rs::transpile_with_options(&wasm, &TranspileOptions::default().with_typed_traps(true))
            .expect("transpile ok");
    assert!(out.contains("Unreachable"), "{out}");
    for unused in [
        "IndirectCallTypeMismatch",
        "CallStackExhausted",
        "IntegerOverflow",
    ] {
        assert!(!out.contains(unused), "{unused} emitted: {out}");
    }
}

#[test]
fn each_helper_trap_unwinds_with_its_typed_variant_and_the_current_message() {
    compile_run_typed(
        "typed_traps_variants",
        TRAPS,
        &format!(
            r#"{CATCH}
            assert_send_static::<Wasm2RsTrap>();
            let mut inst = Instance::new();
            let t = trap_of(|| inst.func1());
            assert!(matches!(t, Wasm2RsTrap::Unreachable));
            assert_eq!(t.to_string(), "unreachable");
            let t = trap_of(|| inst.func2());
            assert!(matches!(t, Wasm2RsTrap::IndirectCallTypeMismatch));
            assert_eq!(t.to_string(), "indirect call type mismatch");
            let t = trap_of(|| inst.func3());
            assert!(matches!(t, Wasm2RsTrap::CallStackExhausted));
            assert_eq!(t.to_string(), "call stack exhausted");
            let t = trap_of(|| inst.func4(f32::NAN));
            assert!(matches!(t, Wasm2RsTrap::InvalidConversionToInteger));
            assert_eq!(t.to_string(), "invalid conversion to integer");
            let t = trap_of(|| inst.func5(1e30));
            assert!(matches!(t, Wasm2RsTrap::IntegerOverflow));
            assert_eq!(t.to_string(), "integer overflow");
            // `resume_unwind` skips the panic hook, so no message is printed.
            assert!(!HOOK_RAN.load(std::sync::atomic::Ordering::SeqCst));
            // In-range conversions still work.
            assert_eq!(inst.func4(2.5), 2);"#
        ),
    );
}

#[test]
fn try_catch_all_does_not_catch_a_typed_trap() {
    compile_run_typed(
        "typed_traps_catch_all",
        TRAPS,
        &format!(
            r#"{CATCH}
            let mut inst = Instance::new();
            assert!(matches!(trap_of(|| inst.func6()), Wasm2RsTrap::Unreachable));
            assert!(matches!(trap_of(|| inst.func7()), Wasm2RsTrap::IndirectCallTypeMismatch));
            assert!(matches!(trap_of(|| inst.func8()), Wasm2RsTrap::CallStackExhausted));
            assert!(matches!(trap_of(|| inst.func9()), Wasm2RsTrap::InvalidConversionToInteger));
            assert!(matches!(trap_of(|| inst.func10()), Wasm2RsTrap::IntegerOverflow));"#
        ),
    );
}

#[test]
fn traps_outside_stage_two_keep_their_string_payload() {
    compile_run_typed(
        "typed_traps_other_panics",
        r#"(module (global (mut i32) (i32.const 0))
            (func (export "i31") (result i32) (i31.get_s (ref.null i31))))"#,
        &format!(
            r#"{CATCH}
            let mut inst = Instance::new();
            let p = payload_of(|| inst.func0());
            assert!(p.downcast_ref::<Wasm2RsTrap>().is_none());
            assert!(p.downcast_ref::<&str>().is_some() || p.downcast_ref::<String>().is_some());"#
        ),
    );
}

/// A wasi-threads child that runs `child_body` while the parent is parked for up
/// to 5s; returns the generated program's stderr after asserting it failed.
fn run_trapping_child(name: &str, child_body: &str) -> String {
    let wat = format!(
        r#"(module
          (import "env" "memory" (memory 1 1 shared))
          (import "wasi" "thread-spawn" (func $spawn (param i32) (result i32)))
          (func (export "wasi_thread_start") (param i32 i32) {child_body})
          (func (export "run") (result i32)
            i32.const 0
            call $spawn
            drop
            i32.const 0
            i32.const 0
            i64.const 5000000000
            memory.atomic.wait32))"#
    );
    let program = format!(
        "{}\nfn main() {{ let mut inst = Instance::new(SharedMemory::new(65536)); inst.func2(); }}\n",
        transpile_typed(&wat, true)
    );
    let bin = common::build(name, &program);
    let run = Command::new(&bin).output().expect("run generated binary");
    assert!(
        !run.status.success(),
        "expected the child trap to end the process"
    );
    String::from_utf8_lossy(&run.stderr).into_owned()
}

#[test]
fn a_typed_trap_in_a_spawned_thread_reports_its_cause() {
    // `resume_unwind` skips the panic hook, so the spawn wrapper is the only
    // place that can print the typed payload.
    let stderr = run_trapping_child("typed_trap_thread_cause", "unreachable");
    assert!(stderr.contains("thread 1 trapped"), "{stderr}");
    assert!(stderr.contains("unreachable"), "{stderr}");
}

#[test]
fn a_string_panic_in_a_typed_spawned_thread_still_reaches_the_hook() {
    // A null `i31.get_s` is a still-untyped string panic; the wrapper must still
    // forward it to the hook even though this shared module defines `Wasm2RsTrap`.
    let stderr = run_trapping_child(
        "typed_trap_thread_string",
        "(if (i31.get_s (ref.null i31)) (then nop))",
    );
    assert!(stderr.contains("thread 1 trapped"), "{stderr}");
    assert!(stderr.contains("null"), "{stderr}");
}

#[test]
fn a_typed_atomic_trap_in_a_spawned_thread_reports_its_cause() {
    let stderr = run_trapping_child(
        "typed_trap_thread_atomic",
        "(drop (i32.atomic.load (i32.const 1)))",
    );
    assert!(stderr.contains("thread 1 trapped"), "{stderr}");
    assert!(stderr.contains("unaligned atomic access"), "{stderr}");
    let stderr = run_trapping_child(
        "typed_trap_thread_atomic_oob",
        "(drop (i32.atomic.load (i32.const 65536)))",
    );
    assert!(stderr.contains("out of bounds memory access"), "{stderr}");
}

/// Typed traps with named exports, so the stage-2 modules are driven by export name.
fn typed_named() -> TranspileOptions {
    TranspileOptions::default()
        .with_typed_traps(true)
        .with_named_exports(true)
}

const DIV_REM: &str = r#"(module
    (global (mut i32) (i32.const 0))
    (func (export "w32_div_s") (param i32 i32) (result i32) (i32.div_s (local.get 0) (local.get 1)))
    (func (export "w32_div_u") (param i32 i32) (result i32) (i32.div_u (local.get 0) (local.get 1)))
    (func (export "w32_rem_s") (param i32 i32) (result i32) (i32.rem_s (local.get 0) (local.get 1)))
    (func (export "w32_rem_u") (param i32 i32) (result i32) (i32.rem_u (local.get 0) (local.get 1)))
    (func (export "w64_div_s") (param i64 i64) (result i64) (i64.div_s (local.get 0) (local.get 1)))
    (func (export "w64_div_u") (param i64 i64) (result i64) (i64.div_u (local.get 0) (local.get 1)))
    (func (export "w64_rem_s") (param i64 i64) (result i64) (i64.rem_s (local.get 0) (local.get 1)))
    (func (export "w64_rem_u") (param i64 i64) (result i64) (i64.rem_u (local.get 0) (local.get 1)))
    (func (export "div_const") (param i32) (result i32) (i32.div_s (local.get 0) (i32.const 3)))
    (func (export "catch_div") (param i32) (result i32)
      try (result i32) (i32.div_u (i32.const 1) (local.get 0)) catch_all i32.const -1 end))"#;

#[test]
fn integer_division_and_remainder_trap_with_typed_variants() {
    compile_run_with(
        "typed_traps_div_rem",
        DIV_REM,
        typed_named(),
        &format!(
            r#"{CATCH}
            let mut inst = Instance::new();
            macro_rules! zero {{ ($f:ident, $a:expr) => {{
                let t = trap_of(|| inst.$f($a, 0));
                assert!(matches!(t, Wasm2RsTrap::IntegerDivideByZero));
                assert_eq!(t.to_string(), "integer divide by zero");
            }} }}
            zero!(w32_div_s, 7); zero!(w32_div_u, 7); zero!(w32_rem_s, 7); zero!(w32_rem_u, 7);
            zero!(w64_div_s, 7); zero!(w64_div_u, 7); zero!(w64_rem_s, 7); zero!(w64_rem_u, 7);
            // `MIN / -1` overflows for div_s only; rem_s yields 0.
            assert!(matches!(trap_of(|| inst.w32_div_s(i32::MIN, -1)), Wasm2RsTrap::IntegerOverflow));
            assert!(matches!(trap_of(|| inst.w64_div_s(i64::MIN, -1)), Wasm2RsTrap::IntegerOverflow));
            assert_eq!(inst.w32_rem_s(i32::MIN, -1), 0);
            assert_eq!(inst.w64_rem_s(i64::MIN, -1), 0);
            // Ordinary and sign-sensitive results match wasm.
            assert_eq!(inst.w32_div_s(-7, 2), -3);
            assert_eq!(inst.w32_rem_s(-7, 2), -1);
            assert_eq!(inst.w32_div_u(-1, 2), 0x7fff_ffff);
            assert_eq!(inst.w32_rem_u(-1, 16), 15);
            assert_eq!(inst.w64_div_s(-7, 2), -3);
            assert_eq!(inst.w64_div_u(-1, 2), i64::MAX);
            assert_eq!(inst.div_const(9), 3);
            // A catch_all does not catch it, and no message is printed.
            assert!(matches!(trap_of(|| inst.catch_div(0)), Wasm2RsTrap::IntegerDivideByZero));
            assert!(!HOOK_RAN.load(std::sync::atomic::Ordering::SeqCst));"#
        ),
    );
}

const MEMORY: &str = r#"(module
    (memory (export "mem") 1 1)
    (data $d "abcd")
    (func (export "load32") (param i32) (result i32) (i32.load (local.get 0)))
    (func (export "load8") (param i32) (result i32) (i32.load8_u (local.get 0)))
    (func (export "load64_off") (param i32) (result i64) (i64.load offset=8 (local.get 0)))
    (func (export "store32") (param i32 i32) (i32.store (local.get 0) (local.get 1)))
    (func (export "store8") (param i32 i32) (i32.store8 (local.get 0) (local.get 1)))
    (func (export "fill") (param i32 i32 i32) (memory.fill (local.get 0) (local.get 1) (local.get 2)))
    (func (export "copy") (param i32 i32 i32) (memory.copy (local.get 0) (local.get 1) (local.get 2)))
    (func (export "init") (param i32 i32 i32) (memory.init $d (local.get 0) (local.get 1) (local.get 2)))
    (func (export "v128") (param i32) (result v128) (v128.load (local.get 0))))"#;

const MEMORY_BODY: &str = r#"
            let mut inst = Instance::new();
            macro_rules! oob { ($e:expr) => {{
                let t = trap_of(|| $e);
                assert!(matches!(t, Wasm2RsTrap::OutOfBoundsMemoryAccess));
                assert_eq!(t.to_string(), "out of bounds memory access");
            }} }
            // The last in-bounds accesses work, one byte further traps.
            inst.store32(65532, 0x01020304);
            assert_eq!(inst.load32(65532), 0x01020304);
            oob!(inst.load32(65533));
            oob!(inst.load32(-1));
            oob!(inst.load8(65536));
            assert_eq!(inst.load8(65535), 1);
            oob!(inst.load64_off(65536 - 8));
            oob!(inst.store32(65533, 1));
            oob!(inst.store8(65536, 1));
            oob!(inst.v128(65521));
            // Bulk operations trap with no partial write.
            inst.fill(0, 7, 4);
            assert_eq!(inst.load32(0), 0x07070707);
            oob!(inst.fill(65535, 9, 2));
            assert_eq!(inst.load8(65535), 1);
            oob!(inst.copy(65535, 0, 2));
            oob!(inst.copy(0, 65535, 2));
            inst.copy(4, 0, 4);
            assert_eq!(inst.load32(4), 0x07070707);
            oob!(inst.init(65535, 0, 4));
            oob!(inst.init(0, 1, 4));
            inst.init(8, 0, 4);
            assert_eq!(inst.load32(8), i32::from_le_bytes(*b"abcd"));"#;

#[test]
fn memory_out_of_bounds_traps_with_a_typed_variant() {
    compile_run_with(
        "typed_traps_memory",
        MEMORY,
        typed_named(),
        &format!("{CATCH}{MEMORY_BODY}"),
    );
}

#[test]
fn unsafe_memory_keeps_division_typed_and_drops_the_memory_checks() {
    let out = transpile_with(
        MEMORY,
        TranspileOptions::default()
            .with_typed_traps(true)
            .with_unsafe_memory(true),
    );
    assert!(out.contains("get_unchecked"), "{out}");
    assert!(!out.contains("trap_memory_oob"), "{out}");
    assert!(!out.contains("OutOfBoundsMemoryAccess"), "{out}");
    compile_run_with(
        "typed_traps_unsafe_memory_div",
        DIV_REM,
        typed_named().with_unsafe_memory(true),
        &format!(
            r#"{CATCH}
            let mut inst = Instance::new();
            assert!(matches!(trap_of(|| inst.w32_div_s(1, 0)), Wasm2RsTrap::IntegerDivideByZero));"#
        ),
    );
}

const TABLE: &str = r#"(module
    (type $v (func (result i32)))
    (table $t 4 4 funcref)
    (table $u 4 4 funcref)
    (elem $e funcref (ref.func $one) (ref.func $one))
    (func $one (type $v) i32.const 1)
    (func (export "call") (param i32) (result i32) (call_indirect (type $v) (local.get 0)))
    (func (export "get") (param i32) (result i32) (ref.is_null (table.get $t (local.get 0))))
    (func (export "set") (param i32) (table.set $t (local.get 0) (ref.func $one)))
    (func (export "fill") (param i32 i32) (table.fill $t (local.get 0) (ref.func $one) (local.get 1)))
    (func (export "copy") (param i32 i32 i32) (table.copy $t $t (local.get 0) (local.get 1) (local.get 2)))
    (func (export "copy_x") (param i32 i32 i32) (table.copy $u $t (local.get 0) (local.get 1) (local.get 2)))
    (func (export "init") (param i32 i32 i32) (table.init $t $e (local.get 0) (local.get 1) (local.get 2))))"#;

#[test]
fn table_out_of_bounds_traps_with_a_typed_variant() {
    compile_run_with(
        "typed_traps_table",
        TABLE,
        typed_named(),
        &format!(
            r#"{CATCH}
            let mut inst = Instance::new();
            macro_rules! oob {{ ($e:expr) => {{
                let t = trap_of(|| $e);
                assert!(matches!(t, Wasm2RsTrap::OutOfBoundsTableAccess));
                assert_eq!(t.to_string(), "out of bounds table access");
            }} }}
            inst.set(3);
            assert_eq!(inst.get(3), 0);
            assert_eq!(inst.call(3), 1);
            oob!(inst.get(4));
            oob!(inst.set(4));
            oob!(inst.call(4));
            oob!(inst.call(-1));
            inst.fill(2, 2);
            oob!(inst.fill(3, 2));
            inst.copy(0, 2, 2);
            oob!(inst.copy(3, 0, 2));
            oob!(inst.copy(0, 3, 2));
            inst.copy_x(0, 0, 4);
            oob!(inst.copy_x(1, 0, 4));
            inst.init(0, 0, 2);
            oob!(inst.init(3, 0, 2));
            oob!(inst.init(0, 1, 2));"#
        ),
    );
}

#[test]
fn off_keeps_stage_two_traps_as_rust_panics_and_emits_no_new_helpers() {
    for wat in [DIV_REM, MEMORY, TABLE] {
        let plain = transpile_typed(wat, false);
        for absent in [
            "Wasm2RsTrap",
            "trap_memory_oob",
            "trap_table_oob",
            "trap_integer_divide_by_zero",
            "i32_div_s(",
        ] {
            assert!(!plain.contains(absent), "{absent} in the untyped output");
        }
        assert_eq!(plain, transpile_with(wat, TranspileOptions::default()));
    }
}

#[test]
fn only_the_used_stage_two_variants_are_emitted() {
    let div = transpile_typed(DIV_REM, true);
    assert!(div.contains("IntegerDivideByZero"), "{div}");
    for unused in ["OutOfBoundsMemoryAccess", "OutOfBoundsTableAccess"] {
        assert!(!div.contains(unused), "{unused} emitted: {div}");
    }
    let mem = transpile_typed(MEMORY, true);
    assert!(mem.contains("OutOfBoundsMemoryAccess"), "{mem}");
    for unused in ["IntegerDivideByZero", "OutOfBoundsTableAccess"] {
        assert!(!mem.contains(unused), "{unused} emitted: {mem}");
    }
}

#[test]
fn cross_memory_copy_and_shared_module_table_helpers_are_typed() {
    compile_run_with(
        "typed_traps_cross_mem",
        r#"(module
            (memory 1 1) (memory $b 1 1)
            (func (export "xcopy") (param i32 i32 i32)
              (memory.copy 0 1 (local.get 0) (local.get 1) (local.get 2))))"#,
        typed_named(),
        &format!(
            r#"{CATCH}
            let mut inst = Instance::new();
            inst.xcopy(0, 0, 4);
            assert!(matches!(trap_of(|| inst.xcopy(65535, 0, 4)), Wasm2RsTrap::OutOfBoundsMemoryAccess));
            assert!(matches!(trap_of(|| inst.xcopy(0, 65535, 4)), Wasm2RsTrap::OutOfBoundsMemoryAccess));"#
        ),
    );
    let shared = transpile_with(
        r#"(module (memory 1 1 shared) (table 2 funcref)
            (func (export "tc") (param i32) (table.copy (local.get 0) (i32.const 0) (i32.const 1))))"#,
        typed_named(),
    );
    assert!(shared.contains("trap_table_oob"), "{shared}");
    assert!(
        !shared.contains("copy_within(s..s + len as usize, d);\n"),
        "{shared}"
    );
}

const SHARED_ATOMICS: &str = r#"(module
    (import "env" "memory" (memory 1 1 shared))
    (func (export "ld32") (param i32) (result i32) (i32.atomic.load (local.get 0)))
    (func (export "ld64") (param i32) (result i64) (i64.atomic.load (local.get 0)))
    (func (export "ld16") (param i32) (result i32) (i32.atomic.load16_u (local.get 0)))
    (func (export "st32") (param i32) (i32.atomic.store (local.get 0) (i32.const 1)))
    (func (export "rmw") (param i32) (result i32) (i32.atomic.rmw.add (local.get 0) (i32.const 1)))
    (func (export "cas") (param i32) (result i32)
      (i32.atomic.rmw.cmpxchg (local.get 0) (i32.const 0) (i32.const 1)))
    (func (export "wait") (param i32) (result i32)
      (memory.atomic.wait32 (local.get 0) (i32.const 1) (i64.const 0)))
    (func (export "plain_ld") (param i32) (result i32) (i32.load (local.get 0)))
    (func (export "plain_st") (param i32) (i32.store (local.get 0) (i32.const 1)))
    (func (export "catch_ld") (param i32) (result i32)
      try (result i32) (i32.atomic.load (local.get 0)) catch_all i32.const -1 end))"#;

#[test]
fn shared_memory_atomics_and_plain_accesses_trap_with_typed_variants() {
    compile_run_with(
        "typed_traps_shared_atomics",
        SHARED_ATOMICS,
        typed_named(),
        &format!(
            r#"{CATCH}
            let mut inst = Instance::new(SharedMemory::new(65536));
            macro_rules! unaligned {{ ($e:expr) => {{
                let t = trap_of(|| $e);
                assert!(matches!(t, Wasm2RsTrap::UnalignedAtomicAccess));
                assert_eq!(t.to_string(), "unaligned atomic access");
            }} }}
            macro_rules! oob {{ ($e:expr) => {{
                let t = trap_of(|| $e);
                assert!(matches!(t, Wasm2RsTrap::OutOfBoundsMemoryAccess));
                assert_eq!(t.to_string(), "out of bounds memory access");
            }} }}
            // In-bounds, aligned accesses are unaffected.
            assert_eq!(inst.ld32(4), 0);
            inst.st32(8);
            assert_eq!(inst.rmw(8), 1);
            assert_eq!(inst.cas(12), 0);
            assert_eq!(inst.wait(8), 1);
            assert_eq!(inst.ld16(2), 0);
            assert_eq!(inst.plain_ld(8), 2);
            // Misaligned: every atomic family, and `wait` (which checks first).
            unaligned!(inst.ld32(1)); unaligned!(inst.ld64(4)); unaligned!(inst.ld16(1));
            unaligned!(inst.st32(2)); unaligned!(inst.rmw(3)); unaligned!(inst.cas(6));
            unaligned!(inst.wait(1));
            // Out of bounds, including a far-out address; alignment is checked first.
            oob!(inst.ld32(65536)); oob!(inst.ld64(65536)); oob!(inst.st32(65540));
            oob!(inst.rmw(0xffff_fffc_u32 as i32)); oob!(inst.cas(65536)); oob!(inst.wait(65536));
            unaligned!(inst.ld32(65537));
            // Plain accesses to a shared memory are typed too.
            oob!(inst.plain_ld(65535)); oob!(inst.plain_st(65535));
            oob!(inst.plain_ld(-1));
            // A catch_all does not catch a typed trap, and no message is printed.
            assert!(matches!(trap_of(|| inst.catch_ld(1)), Wasm2RsTrap::UnalignedAtomicAccess));
            assert!(!HOOK_RAN.load(std::sync::atomic::Ordering::SeqCst));"#
        ),
    );
}

#[test]
fn shared_memory_traps_stay_rust_panics_when_off() {
    let plain = transpile_typed(SHARED_ATOMICS, false);
    for absent in [
        "Wasm2RsTrap",
        "trap_memory_oob",
        "trap_unaligned_atomic_access",
    ] {
        assert!(!plain.contains(absent), "{absent} in the untyped output");
    }
    assert!(
        plain.contains(r#"panic!("unaligned atomic access")"#),
        "{plain}"
    );
    assert!(
        plain.contains(r#"panic!("out of bounds atomic access")"#),
        "{plain}"
    );
    let typed = transpile_typed(SHARED_ATOMICS, true);
    assert!(!typed.contains("panic!("), "{typed}");
    assert!(typed.contains("UnalignedAtomicAccess"), "{typed}");
    assert!(typed.contains("OutOfBoundsMemoryAccess"), "{typed}");
}

#[test]
fn a_split_shared_module_defines_its_typed_traps_and_compiles() {
    let wasm = wat::parse_str(
        r#"(module (memory 1 1 shared)
            (func (export "a") (param i32) (result i32) (i32.atomic.load (local.get 0)))
            (func (export "b") (param i32) (result i32) (i32.load (local.get 0))))"#,
    )
    .expect("valid wat");
    let split = wasm2rs::SplitOptions {
        funcs_per_file: 1,
        max_bytes_per_file: 0,
    };
    let mut files = Vec::new();
    wasm2rs::transpile_split_with_options(&wasm, &split, &typed_named(), |f| {
        files.push(f);
        Ok(())
    })
    .expect("transpile ok");
    assert!(files.len() > 1, "expected a split output");
    let dir = common::scratch_dir("typed-traps-split-shared");
    for f in &files {
        let mut code = f.code.clone();
        if f.name == "lib.rs" {
            code.push_str(&format!(
                "{CATCH}\nfn main() {{ let mut inst = Instance::new();\n\
                 assert!(matches!(trap_of(|| inst.a(1)), Wasm2RsTrap::UnalignedAtomicAccess));\n\
                 assert!(matches!(trap_of(|| inst.b(65536)), Wasm2RsTrap::OutOfBoundsMemoryAccess)); }}\n"
            ));
        }
        std::fs::write(dir.join(&f.name), code).expect("write generated file");
    }
    let bin = dir.join("gen");
    let out = Command::new("rustc")
        .current_dir(&dir)
        .args(["lib.rs", "--edition", "2024", "-o"])
        .arg(&bin)
        .output()
        .expect("run rustc");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(Command::new(&bin).status().expect("run").success());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn memory_init_on_a_shared_memory_traps_typed_for_both_ranges() {
    compile_run_with(
        "typed_traps_shared_init",
        r#"(module (memory 1 1 shared) (data $d "abcd")
            (func (export "init") (param i32 i32 i32)
              (memory.init $d (local.get 0) (local.get 1) (local.get 2))))"#,
        typed_named(),
        &format!(
            r#"{CATCH}
            let mut inst = Instance::new();
            inst.init(0, 0, 4);
            assert!(matches!(trap_of(|| inst.init(0, 1, 4)), Wasm2RsTrap::OutOfBoundsMemoryAccess));
            assert!(matches!(trap_of(|| inst.init(65535, 0, 4)), Wasm2RsTrap::OutOfBoundsMemoryAccess));"#
        ),
    );
}
