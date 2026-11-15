//! Tests for the opt-in `TranspileOptions::typed_traps` (M6 stage 1).
//!
//! With the option on, the five runtime-helper traps (`unreachable`, indirect call
//! type mismatch, call stack exhausted and the two float->int conversion traps)
//! unwind with a `Wasm2RsTrap` payload (`resume_unwind`, so the panic hook never
//! runs) instead of a message string, the same mechanism a wasm exception uses.
//! Stage 2 adds integer division/remainder and linear-memory / table out-of-bounds
//! traps (explicit checks in place of Rust's `/` and slice indexing). Stage 3a adds
//! the shared-memory atomics (a misaligned or out-of-bounds atomic access) and the
//! plain accesses of a shared memory. Stage 3b adds the GC traps (null reference,
//! cast failure, array bounds) and a null `exnref`. The remaining traps
//! (continuations, glue) keep their string payload, and the option is off by default.

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
        // On an unshared memory a `wait` whose value matches would block forever, a
        // still-untyped string panic.
        r#"(module (memory 1)
            (func (export "wait") (result i32)
              (memory.atomic.wait32 (i32.const 0) (i32.const 0) (i64.const -1))))"#,
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
            // A negative i32 operand is a huge unsigned one: a trap, not an
            // overflow of `d + n`.
            oob!(inst.init(-1, 0, 1));
            oob!(inst.init(0, -1, 1));
            oob!(inst.init(0, 0, -1));
            oob!(inst.init(-1, -1, -1));
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
            oob!(inst.init(0, 1, 2));
            oob!(inst.init(-1, 0, 1));
            oob!(inst.init(0, -1, 1));
            oob!(inst.init(0, 0, -1));
            oob!(inst.init(-1, -1, -1));"#
        ),
    );
}

const GC_TABLE: &str = r#"(module
    (table $t 4 4 exnref)
    (elem $e exnref (ref.null exn) (ref.null exn))
    (func (export "init") (param i32 i32 i32) (table.init $t $e (local.get 0) (local.get 1) (local.get 2))))"#;

#[test]
fn gc_table_init_traps_on_negative_operands() {
    compile_run_with(
        "typed_traps_gc_table_init",
        GC_TABLE,
        typed_named(),
        &format!(
            r#"{CATCH}
            let mut inst = Instance::new();
            macro_rules! oob {{ ($e:expr) => {{
                assert!(matches!(trap_of(|| $e), Wasm2RsTrap::OutOfBoundsTableAccess));
            }} }}
            inst.init(0, 0, 2);
            oob!(inst.init(3, 0, 2));
            oob!(inst.init(-1, 0, 1));
            oob!(inst.init(0, -1, 1));
            oob!(inst.init(0, 0, -1));"#
        ),
    );
}

const MEMORY64: &str = r#"(module
    (memory i64 1 1)
    (data $d "abcd")
    (func (export "init") (param i64 i32 i32) (memory.init $d (local.get 0) (local.get 1) (local.get 2))))"#;

#[test]
fn memory64_init_traps_on_a_negative_destination() {
    compile_run_with(
        "typed_traps_memory64_init",
        MEMORY64,
        typed_named(),
        &format!(
            r#"{CATCH}
            let mut inst = Instance::new();
            macro_rules! oob {{ ($e:expr) => {{
                assert!(matches!(trap_of(|| $e), Wasm2RsTrap::OutOfBoundsMemoryAccess));
            }} }}
            inst.init(0, 0, 4);
            oob!(inst.init(-1, 0, 1));
            oob!(inst.init(65535, 0, 4));
            oob!(inst.init(0, -1, 1));
            oob!(inst.init(0, 0, -1));"#
        ),
    );
}

#[test]
fn table64_init_traps_on_a_negative_destination() {
    compile_run_with(
        "typed_traps_table64_init",
        r#"(module
            (table $t i64 4 4 funcref)
            (elem $e funcref (ref.null func) (ref.null func))
            (func (export "init") (param i64 i32 i32) (table.init $t $e (local.get 0) (local.get 1) (local.get 2))))"#,
        typed_named(),
        &format!(
            r#"{CATCH}
            let mut inst = Instance::new();
            macro_rules! oob {{ ($e:expr) => {{
                assert!(matches!(trap_of(|| $e), Wasm2RsTrap::OutOfBoundsTableAccess));
            }} }}
            inst.init(0, 0, 2);
            oob!(inst.init(-1, 0, 1));
            oob!(inst.init(3, 0, 2));
            oob!(inst.init(0, -1, 1));"#
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
            assert!(matches!(trap_of(|| inst.xcopy(0, 65535, 4)), Wasm2RsTrap::OutOfBoundsMemoryAccess));
            assert!(matches!(trap_of(|| inst.xcopy(-1, 0, 1)), Wasm2RsTrap::OutOfBoundsMemoryAccess));
            assert!(matches!(trap_of(|| inst.xcopy(0, -1, 1)), Wasm2RsTrap::OutOfBoundsMemoryAccess));
            assert!(matches!(trap_of(|| inst.xcopy(0, 0, -1)), Wasm2RsTrap::OutOfBoundsMemoryAccess));"#
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
    compile_run_split_typed(
        "typed-traps-split-shared",
        r#"(module (memory 1 1 shared)
            (func (export "a") (param i32) (result i32) (i32.atomic.load (local.get 0)))
            (func (export "b") (param i32) (result i32) (i32.load (local.get 0))))"#,
        "let mut inst = Instance::new();\n\
         assert!(matches!(trap_of(|| inst.a(1)), Wasm2RsTrap::UnalignedAtomicAccess));\n\
         assert!(matches!(trap_of(|| inst.b(65536)), Wasm2RsTrap::OutOfBoundsMemoryAccess));",
    );
}

/// Transpile `wat` split one function per file with typed traps, append [`CATCH`]
/// and a `main` running `main_body` to the crate root, then compile and run it.
fn compile_run_split_typed(dir_name: &str, wat: &str, main_body: &str) {
    let wasm = wat::parse_str(wat).expect("valid wat");
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
    let dir = common::scratch_dir(dir_name);
    for f in &files {
        let mut code = f.code.clone();
        if f.name == "lib.rs" {
            code.push_str(&format!("{CATCH}\nfn main() {{\n{main_body} }}\n"));
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

#[test]
fn notify_traps_are_typed_on_shared_and_unshared_memory() {
    // `memory.atomic.notify` checks alignment and bounds on both kinds of memory;
    // under `typed_traps` the alignment trap is `UnalignedAtomicAccess` either way.
    let wat = |mem: &str| {
        format!(
            r#"(module (memory 1 1 {mem})
                (func (export "notify") (param i32) (result i32)
                  local.get 0 i32.const 1 memory.atomic.notify))"#
        )
    };
    for (name, mem) in [
        ("typed_notify_unshared", ""),
        ("typed_notify_shared", "shared"),
    ] {
        compile_run_with(
            name,
            &wat(mem),
            typed_named(),
            &format!(
                r#"{CATCH}
                let mut inst = Instance::new();
                assert_eq!(inst.notify(8), 0);
                assert!(matches!(trap_of(|| inst.notify(2)), Wasm2RsTrap::UnalignedAtomicAccess));"#
            ),
        );
    }
}

/// Every atomic access family and width, on an unshared memory of either address
/// width. Returns the wat and `(export name, access bytes)` of each function; all
/// take one address parameter.
fn unshared_atomics(memory64: bool) -> (String, Vec<(String, u32)>) {
    let aty = if memory64 { "i64" } else { "i32" };
    let mut funcs = String::new();
    let mut names = Vec::new();
    for (vty, bytes, load, store, rmw, cas) in [
        (
            "i32",
            1,
            "load8_u",
            "store8",
            "rmw8.add_u",
            "rmw8.cmpxchg_u",
        ),
        (
            "i32",
            2,
            "load16_u",
            "store16",
            "rmw16.add_u",
            "rmw16.cmpxchg_u",
        ),
        ("i32", 4, "load", "store", "rmw.add", "rmw.cmpxchg"),
        (
            "i64",
            1,
            "load8_u",
            "store8",
            "rmw8.add_u",
            "rmw8.cmpxchg_u",
        ),
        (
            "i64",
            2,
            "load16_u",
            "store16",
            "rmw16.add_u",
            "rmw16.cmpxchg_u",
        ),
        (
            "i64",
            4,
            "load32_u",
            "store32",
            "rmw32.add_u",
            "rmw32.cmpxchg_u",
        ),
        ("i64", 8, "load", "store", "rmw.add", "rmw.cmpxchg"),
    ] {
        for (kind, op) in [("ld", load), ("st", store), ("rmw", rmw), ("cas", cas)] {
            let name = format!("{kind}_{vty}_{}", op.replace('.', "_"));
            let access = format!("{vty}.atomic.{op}");
            let body = match kind {
                "ld" => format!("(drop ({access} (local.get 0)))"),
                "st" => format!("({access} (local.get 0) ({vty}.const 1))"),
                "rmw" => format!("(drop ({access} (local.get 0) ({vty}.const 1)))"),
                _ => format!("(drop ({access} (local.get 0) ({vty}.const 0) ({vty}.const 1)))"),
            };
            funcs.push_str(&format!(
                "(func (export \"{name}\") (param {aty}) {body})\n"
            ));
            names.push((name, bytes));
        }
    }
    let mem = if memory64 {
        "(memory i64 1 1)"
    } else {
        "(memory 1 1)"
    };
    (format!("(module {mem}\n{funcs})"), names)
}

/// A `msg_of` helper for untyped builds: the string payload of a trap's panic.
const MSG_OF: &str = r#"
fn msg_of<R>(f: impl FnOnce() -> R) -> String {
    let p = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).err().expect("a trap");
    p.downcast_ref::<&str>().map(|s| s.to_string())
        .or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_default()
}
"#;

fn unshared_atomic_main(names: &[(String, u32)], typed: bool) -> String {
    // `CATCH` names `Wasm2RsTrap`, which only a typed build defines.
    let mut body = String::from(if typed { CATCH } else { MSG_OF });
    body.push_str("let mut inst = Instance::new();\n");
    for (name, bytes) in names {
        // An aligned access is unaffected; a byte access can never be misaligned.
        body.push_str(&format!("inst.{name}(8);\n"));
        if *bytes == 1 {
            body.push_str(&format!("inst.{name}(9);\n"));
            continue;
        }
        let bad = 8 + bytes / 2;
        body.push_str(&if typed {
            format!(
                "assert!(matches!(trap_of(|| inst.{name}({bad})), Wasm2RsTrap::UnalignedAtomicAccess), \"{name}\");\n"
            )
        } else {
            format!(
                "assert_eq!(msg_of(|| inst.{name}({bad})), \"unaligned atomic access\", \"{name}\");\n"
            )
        });
    }
    body
}

#[test]
fn unshared_memory_atomics_trap_when_misaligned_at_every_width() {
    // The threads proposal requires the alignment trap on every atomic access,
    // shared memory or not; an aligned access keeps working.
    for (memory64, typed) in [(false, false), (false, true), (true, false), (true, true)] {
        let (wat, names) = unshared_atomics(memory64);
        let opts = TranspileOptions::default()
            .with_typed_traps(typed)
            .with_named_exports(true);
        compile_run_with(
            &format!("unshared_atomic_align_{memory64}_{typed}"),
            &wat,
            opts,
            &unshared_atomic_main(&names, typed),
        );
    }
}

#[test]
fn unshared_atomic_alignment_includes_the_static_offset() {
    compile_run_with(
        "unshared_atomic_align_offset",
        r#"(module (memory 1 1)
            (func (export "ld") (param i32) (result i32) (i32.atomic.load offset=2 (local.get 0))))"#,
        typed_named(),
        &format!(
            r#"{CATCH}
            let mut inst = Instance::new();
            assert_eq!(inst.ld(2), 0);
            assert!(matches!(trap_of(|| inst.ld(0)), Wasm2RsTrap::UnalignedAtomicAccess));"#
        ),
    );
}

#[test]
fn unshared_atomic_store_evaluates_its_value_before_the_alignment_trap() {
    // The stored value traps (out of bounds) while the store address is also
    // misaligned: the operand runs first, so its trap wins.
    compile_run_with(
        "unshared_atomic_align_order",
        r#"(module (memory 1 1)
            (func (export "st") (param i32)
              (i32.atomic.store (i32.const 2) (i32.load (local.get 0)))))"#,
        typed_named(),
        &format!(
            r#"{CATCH}
            let mut inst = Instance::new();
            assert!(matches!(trap_of(|| inst.st(65536)), Wasm2RsTrap::OutOfBoundsMemoryAccess));
            assert!(matches!(trap_of(|| inst.st(0)), Wasm2RsTrap::UnalignedAtomicAccess));"#
        ),
    );
}

/// Stage 3b: GC (null reference, cast failure, array bounds, `i31`) and `exnref` traps.
const GC_TRAPS: &str = r#"(module
    (type $s (struct (field (mut i32))))
    (type $a (array (mut i32)))
    (type $b (struct (field i64)))
    (tag $e)
    (elem $seg func $f)
    (data $d "\01\02\03\04")
    (func $f)
    (func (export "s_get") (param (ref null $s)) (result i32) (struct.get $s 0 (local.get 0)))
    (func (export "s_set") (param (ref null $s)) (struct.set $s 0 (local.get 0) (i32.const 1)))
    (func (export "a_len") (param (ref null $a)) (result i32) (array.len (local.get 0)))
    (func (export "a_new") (result (ref $a)) (array.new_default $a (i32.const 2)))
    (func (export "a_get") (param (ref null $a) i32) (result i32)
      (array.get $a (local.get 0) (local.get 1)))
    (func (export "a_set") (param (ref null $a) i32)
      (array.set $a (local.get 0) (local.get 1) (i32.const 7)))
    (func (export "a_fill") (param (ref null $a) i32 i32)
      (array.fill $a (local.get 0) (local.get 1) (i32.const 7) (local.get 2)))
    (func (export "a_copy") (param (ref null $a) i32 i32)
      (array.copy $a $a (local.get 0) (local.get 1) (local.get 0) (i32.const 0) (local.get 2)))
    (func (export "a_init_data") (param (ref null $a) i32 i32 i32)
      (array.init_data $a $d (local.get 0) (local.get 1) (local.get 2) (local.get 3)))
    (func (export "a_new_data") (param i32 i32) (result (ref $a))
      (array.new_data $a $d (local.get 0) (local.get 1)))
    (func (export "cast") (param anyref) (result (ref $s)) (ref.cast (ref $s) (local.get 0)))
    (func (export "mk_b") (result anyref) (struct.new $b (i64.const 1)))
    (func (export "mk_null") (result anyref) (ref.null any))
    (func (export "non_null") (param anyref) (result (ref any)) (ref.as_non_null (local.get 0)))
    (func (export "i31_get") (param i31ref) (result i32) (i31.get_s (local.get 0)))
    (func (export "i31_get_u") (param i31ref) (result i32) (i31.get_u (local.get 0)))
    (func (export "throw_null") (throw_ref (ref.null exn)))
    (func (export "c_null") (result i32)
      try (result i32) (i32.const 1) (call $nul) (drop) catch_all i32.const 0 end)
    (func $nul (result i32) (struct.get $s 0 (ref.null $s))))"#;

#[test]
fn gc_off_by_default_keeps_string_traps() {
    let plain = transpile_with(
        GC_TRAPS,
        TranspileOptions::default().with_named_exports(true),
    );
    assert!(!plain.contains("Wasm2RsTrap"), "{plain}");
    assert_eq!(
        plain,
        transpile_with(
            GC_TRAPS,
            TranspileOptions::default()
                .with_named_exports(true)
                .with_typed_traps(false)
        )
    );
}

#[test]
fn gc_null_dereferences_are_typed() {
    compile_run_with(
        "typed_gc_null",
        GC_TRAPS,
        typed_named(),
        &format!(
            r#"{CATCH}
            let mut inst = Instance::new();
            let null = || GcRef::Null;
            let t = trap_of(|| inst.s_get(null()));
            assert!(matches!(t, Wasm2RsTrap::NullReference));
            assert_eq!(t.to_string(), "null reference");
            assert!(matches!(trap_of(|| inst.s_set(null())), Wasm2RsTrap::NullReference));
            assert!(matches!(trap_of(|| inst.a_len(null())), Wasm2RsTrap::NullReference));
            assert!(matches!(trap_of(|| inst.a_get(null(), 0)), Wasm2RsTrap::NullReference));
            assert!(matches!(trap_of(|| inst.a_set(null(), 0)), Wasm2RsTrap::NullReference));
            assert!(matches!(trap_of(|| inst.a_fill(null(), 0, 0)), Wasm2RsTrap::NullReference));
            assert!(matches!(trap_of(|| inst.a_copy(null(), 0, 0)), Wasm2RsTrap::NullReference));
            assert!(matches!(trap_of(|| inst.a_init_data(null(), 0, 0, 0)), Wasm2RsTrap::NullReference));
            assert!(matches!(trap_of(|| inst.non_null(null())), Wasm2RsTrap::NullReference));
            assert!(matches!(trap_of(|| inst.i31_get(null())), Wasm2RsTrap::NullReference));
            assert!(matches!(trap_of(|| inst.i31_get_u(null())), Wasm2RsTrap::NullReference));
            assert!(matches!(trap_of(|| inst.throw_null()), Wasm2RsTrap::NullReference));
            // A typed trap is not caught by `catch_all`, and the hook never ran.
            assert!(matches!(trap_of(|| inst.c_null()), Wasm2RsTrap::NullReference));
            assert!(!HOOK_RAN.load(std::sync::atomic::Ordering::SeqCst));"#
        ),
    );
}

#[test]
fn gc_cast_failure_and_array_bounds_are_typed() {
    compile_run_with(
        "typed_gc_cast_array",
        GC_TRAPS,
        typed_named(),
        &format!(
            r#"{CATCH}
            let mut inst = Instance::new();
            let b = inst.mk_b();
            let t = trap_of(|| inst.cast(b));
            assert!(matches!(t, Wasm2RsTrap::CastFailure));
            assert_eq!(t.to_string(), "cast failure");
            let nul = inst.mk_null();
            assert!(matches!(trap_of(|| inst.cast(nul)), Wasm2RsTrap::CastFailure));
            let arr = inst.a_new();
            assert_eq!(inst.a_get(arr.clone(), 1), 0);
            let t = trap_of(|| inst.a_get(arr.clone(), 2));
            assert!(matches!(t, Wasm2RsTrap::OutOfBoundsArrayAccess));
            assert_eq!(t.to_string(), "out of bounds array access");
            assert!(matches!(trap_of(|| inst.a_get(arr.clone(), -1)), Wasm2RsTrap::OutOfBoundsArrayAccess));
            assert!(matches!(trap_of(|| inst.a_set(arr.clone(), 2)), Wasm2RsTrap::OutOfBoundsArrayAccess));
            inst.a_fill(arr.clone(), 1, 1);
            assert!(matches!(trap_of(|| inst.a_fill(arr.clone(), 1, 2)), Wasm2RsTrap::OutOfBoundsArrayAccess));
            assert!(matches!(trap_of(|| inst.a_fill(arr.clone(), 3, 0)), Wasm2RsTrap::OutOfBoundsArrayAccess));
            assert!(matches!(trap_of(|| inst.a_copy(arr.clone(), 1, 2)), Wasm2RsTrap::OutOfBoundsArrayAccess));
            // The array range is checked first, then the segment range.
            assert!(matches!(trap_of(|| inst.a_init_data(arr.clone(), 2, 0, 1)), Wasm2RsTrap::OutOfBoundsArrayAccess));
            // A data segment overrun is the memory trap, not an array one.
            assert!(matches!(trap_of(|| inst.a_init_data(arr.clone(), 0, 0, 2)), Wasm2RsTrap::OutOfBoundsMemoryAccess));
            assert!(matches!(trap_of(|| inst.a_new_data(0, 2)), Wasm2RsTrap::OutOfBoundsMemoryAccess));
            assert!(!HOOK_RAN.load(std::sync::atomic::Ordering::SeqCst));"#
        ),
    );
}

#[test]
fn gc_elem_segment_overrun_is_the_table_trap() {
    compile_run_with(
        "typed_gc_elem",
        r#"(module
            (type $a (array (mut funcref)))
            (elem $seg func $f)
            (func $f)
            (func (export "new_elem") (param i32 i32) (result (ref $a))
              (array.new_elem $a $seg (local.get 0) (local.get 1)))
            (func (export "init_elem") (param (ref null $a) i32 i32 i32)
              (array.init_elem $a $seg (local.get 0) (local.get 1) (local.get 2) (local.get 3))))"#,
        typed_named(),
        &format!(
            r#"{CATCH}
            let mut inst = Instance::new();
            let arr = inst.new_elem(0, 1);
            assert!(matches!(trap_of(|| inst.new_elem(0, 2)), Wasm2RsTrap::OutOfBoundsTableAccess));
            assert!(matches!(trap_of(|| inst.init_elem(arr.clone(), 0, 0, 2)), Wasm2RsTrap::OutOfBoundsArrayAccess));
            assert!(matches!(trap_of(|| inst.init_elem(arr.clone(), 0, 1, 1)), Wasm2RsTrap::OutOfBoundsTableAccess));"#
        ),
    );
}

#[test]
fn gc_traps_only_emit_the_used_variants() {
    let out = transpile_typed(
        r#"(module (type $s (struct (field i32)))
            (func (export "g") (param (ref null $s)) (result i32) (struct.get $s 0 (local.get 0))))"#,
        true,
    );
    assert!(out.contains("NullReference"), "{out}");
    for unused in ["CastFailure", "OutOfBoundsArrayAccess"] {
        assert!(!out.contains(unused), "{unused} emitted: {out}");
    }
}

#[test]
fn a_typed_gc_trap_in_a_spawned_thread_reports_its_cause() {
    let stderr = run_trapping_child(
        "typed_trap_thread_gc",
        "(if (i31.get_s (ref.null i31)) (then nop))",
    );
    assert!(stderr.contains("thread 1 trapped"), "{stderr}");
    assert!(stderr.contains("null reference"), "{stderr}");
}

#[test]
fn a_split_gc_module_defines_its_typed_traps_and_compiles() {
    // The managed value model is emitted at the crate root of a split output, so the
    // cold `trap_null_reference` it calls must be defined there too.
    compile_run_split_typed(
        "typed-traps-split-gc",
        r#"(module (type $s (struct (field i32)))
            (func (export "a") (param (ref null $s)) (result i32) (struct.get $s 0 (local.get 0)))
            (func (export "b") (param anyref) (result (ref $s)) (ref.cast (ref $s) (local.get 0))))"#,
        "assert!(matches!(trap_of(|| a(GcRef::Null)), Wasm2RsTrap::NullReference));\n\
         assert!(matches!(trap_of(|| b(GcRef::Null)), Wasm2RsTrap::CastFailure));",
    );
}

/// #412 / #415: operands left inline that read the same array, and `array.copy`
/// null-check order. Each export takes the array in a local so the operands stay
/// unmaterialised expressions.
const GC_ARRAY_OPERANDS: &str = r#"(module
    (type $a (array (mut i32)))
    (type $f (array (mut funcref)))
    (elem $seg func $fn)
    (data $d "\01\02\03\04")
    (memory 1)
    (func $fn)
    (func (export "mk") (result (ref $a)) (array.new_default $a (i32.const 4)))
    (func (export "mk_f") (result (ref $f)) (array.new_default $f (i32.const 4)))
    (func (export "get") (param (ref $a) i32) (result i32)
      (array.get $a (local.get 0) (local.get 1)))
    (func (export "set_last") (param (ref $a))
      (array.set $a (local.get 0)
        (i32.sub (array.len (local.get 0)) (i32.const 1)) (i32.const 9)))
    (func (export "set_val") (param (ref $a))
      (array.set $a (local.get 0) (i32.const 0) (array.len (local.get 0))))
    (func (export "fill_all") (param (ref $a))
      (array.fill $a (local.get 0) (i32.const 0) (i32.const 7) (array.len (local.get 0))))
    (func (export "copy_len") (param (ref $a) (ref $a))
      (array.copy $a $a (local.get 0) (i32.sub (array.len (local.get 0)) (i32.const 2))
        (local.get 1) (i32.const 0) (i32.const 2)))
    (func (export "init_data_len") (param (ref $a))
      (array.init_data $a $d (local.get 0) (i32.sub (array.len (local.get 0)) (i32.const 1))
        (i32.const 0) (i32.const 1)))
    (func (export "init_elem_len") (param (ref $f))
      (array.init_elem $f $seg (local.get 0) (i32.sub (array.len (local.get 0)) (i32.const 1))
        (i32.const 0) (i32.const 1)))
    (func (export "copy_oob_operand") (param (ref null $a) (ref $a))
      (array.copy $a $a (local.get 0) (i32.const 0) (local.get 1)
        (i32.load (i32.const 65536)) (i32.const 1)))
    (func (export "copy") (param (ref null $a) i32 (ref null $a) i32 i32)
      (array.copy $a $a (local.get 0) (local.get 1) (local.get 2) (local.get 3) (local.get 4))))"#;

#[test]
fn gc_array_ops_with_operands_reading_the_array_do_not_double_borrow() {
    for (typed, name) in [
        (true, "gc_array_operands_on"),
        (false, "gc_array_operands_off"),
    ] {
        compile_run_with(
            name,
            GC_ARRAY_OPERANDS,
            typed_named().with_typed_traps(typed),
            r#"let mut inst = Instance::new();
            let arr = inst.mk();
            inst.set_last(arr.clone());
            assert_eq!(inst.get(arr.clone(), 3), 9);
            inst.set_val(arr.clone());
            assert_eq!(inst.get(arr.clone(), 0), 4);
            inst.fill_all(arr.clone());
            assert_eq!(inst.get(arr.clone(), 3), 7);
            let src = inst.mk();
            inst.set_last(src.clone());
            inst.copy_len(arr.clone(), src.clone());
            assert_eq!(inst.get(arr.clone(), 2), 0);
            inst.init_data_len(arr.clone());
            assert_eq!(inst.get(arr.clone(), 3), 0x0403_0201);
            let funcs = inst.mk_f();
            inst.init_elem_len(funcs);"#,
        );
    }
}

#[test]
fn gc_array_copy_checks_both_nulls_before_any_range() {
    // A null operand with an out-of-range source/destination is a null trap, not a
    // bounds trap, whichever operand is null.
    let calls = r#"let mut inst = Instance::new();
            let arr = inst.mk();
            let p = payload_of(|| inst.copy(GcRef::Null, 0, arr.clone(), 100, 1));
            let p2 = payload_of(|| inst.copy(arr.clone(), 0, GcRef::Null, 100, 1));
            let p3 = payload_of(|| inst.copy(GcRef::Null, 100, arr.clone(), 0, 1));"#;
    compile_run_with(
        "gc_copy_null_on",
        GC_ARRAY_OPERANDS,
        typed_named(),
        &format!(
            r#"{CATCH}
            {calls}
            for p in [p, p2, p3] {{
                assert!(matches!(
                    *p.downcast::<Wasm2RsTrap>().expect("typed"),
                    Wasm2RsTrap::NullReference
                ));
            }}
            // Operands are evaluated first: a trapping operand wins over the null dest.
            let arr = inst.mk();
            assert!(matches!(
                trap_of(|| inst.copy_oob_operand(GcRef::Null, arr.clone())),
                Wasm2RsTrap::OutOfBoundsMemoryAccess
            ));"#
        ),
    );
    compile_run_with(
        "gc_copy_null_off",
        GC_ARRAY_OPERANDS,
        typed_named().with_typed_traps(false),
        // `CATCH` references `Wasm2RsTrap`, which is not generated with typed traps off.
        &format!(
            r#"
            fn payload_of<R>(f: impl FnOnce() -> R) -> Box<dyn std::any::Any + Send> {{
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {{
                    Err(p) => p,
                    Ok(_) => panic!("expected a trap"),
                }}
            }}
            {calls}
            for p in [p, p2, p3] {{
                let m = p.downcast_ref::<&str>().map(|s| s.to_string())
                    .or_else(|| p.downcast_ref::<String>().cloned()).expect("message");
                assert_eq!(m, "null reference");
            }}
            let arr = inst.mk();
            let p = payload_of(|| inst.copy_oob_operand(GcRef::Null, arr.clone()));
            let m = p.downcast_ref::<&str>().map(|s| s.to_string())
                .or_else(|| p.downcast_ref::<String>().cloned()).expect("message");
            assert_ne!(m, "null reference");"#
        ),
    );
}

/// #413: a discarded `i31.get_*` of a null still traps, and before any later
/// side effect (it used to be treated as a pure, droppable read).
const I31_DISCARDED: &str = r#"(module
    (global $g (mut i32) (i32.const 0))
    (func (export "drop_get_u") (param i31ref) (drop (i31.get_u (local.get 0))))
    (func (export "drop_get_s") (param i31ref) (drop (i31.get_s (local.get 0))))
    (func (export "get_then_set") (param i31ref)
      (local.get 0) (i31.get_u) (global.set $g (i32.const 1)) (drop))
    (func (export "g") (result i32) (global.get $g))
    (func (export "mk") (result i31ref) (ref.i31 (i32.const 5))))"#;

#[test]
fn a_discarded_i31_get_of_null_still_traps_in_both_modes() {
    for typed in [false, true] {
        // `CATCH` names `Wasm2RsTrap`, which only a typed build defines.
        let prelude = if typed {
            format!(
                "{CATCH}\nfn null_trap<R>(f: impl FnOnce() -> R) {{ \
                 assert!(matches!(trap_of(f), Wasm2RsTrap::NullReference)); }}"
            )
        } else {
            r#"fn null_trap<R>(f: impl FnOnce() -> R) {
                std::panic::set_hook(Box::new(|_| {}));
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
                let _ = std::panic::take_hook();
                assert!(r.is_err(), "expected a trap");
            }"#
            .to_string()
        };
        compile_run_with(
            &format!("i31_discarded_{typed}"),
            I31_DISCARDED,
            typed_named().with_typed_traps(typed),
            &format!(
                r#"{prelude}
            let mut inst = Instance::new();
            null_trap(|| inst.drop_get_u(GcRef::Null));
            null_trap(|| inst.drop_get_s(GcRef::Null));
            null_trap(|| inst.get_then_set(GcRef::Null));
            // The trap fired before the later `global.set`.
            assert_eq!(inst.g(), 0);
            // A non-null operand still evaluates normally.
            let v = inst.mk();
            inst.drop_get_u(v.clone());
            inst.drop_get_s(v.clone());
            inst.get_then_set(v);
            assert_eq!(inst.g(), 1);"#
            ),
        );
    }
}

/// #414: the `array.new*` size is an unsigned i32, so it is widened with
/// `as u32 as usize` like every other GC length (sign extension turned `-1`
/// into `usize::MAX`, a std "capacity overflow" panic).
#[test]
fn array_new_widens_its_size_as_unsigned() {
    let wat = r#"(module
        (memory 1)
        (type $a (array (mut i32)))
        (func (export "n") (param i32) (result (ref $a))
          (array.new $a (i32.const 9) (local.get 0)))
        (func (export "nd") (param i32) (result (ref $a))
          (array.new_default $a (local.get 0)))
        (func (export "len") (param (ref null $a)) (result i32) (array.len (local.get 0))))"#;
    for typed in [false, true] {
        let out = transpile_with(wat, typed_named().with_typed_traps(typed));
        assert!(!out.contains(") as usize; GcRef::Obj"), "{out}");
        assert_eq!(
            out.matches("as u32 as usize; GcRef::Obj").count(),
            2,
            "{out}"
        );
    }
    compile_run_with(
        "array_new_size_unsigned",
        wat,
        typed_named(),
        r#"let mut inst = Instance::new();
        let a = inst.n(3);
        assert_eq!(inst.len(a), 3);
        let b = inst.nd(0);
        assert_eq!(inst.len(b), 0);"#,
    );
}

/// `call_ref` of a null funcref and `any.convert_extern` of a host-supplied
/// externref that is not a handle `extern.convert_any` produced (#416).
const NULL_FUNCREF_AND_EXTERN: &str = r#"(module
    (type $ft (func (result i32)))
    (elem declare func $f)
    (func $f (result i32) i32.const 7)
    (func (export "call_ok") (result i32) (call_ref $ft (ref.func $f)))
    (func (export "call_null") (result i32) (call_ref $ft (ref.null $ft)))
    (func (export "call_param") (param (ref null $ft)) (result i32) (call_ref $ft (local.get 0)))
    (func (export "tail_null") (result i32) (return_call_ref $ft (ref.null $ft)))
    (func (export "c_null") (result i32)
      try (result i32) (call_ref $ft (ref.null $ft)) catch_all i32.const 0 end)
    (func (export "conv") (param externref) (result i32)
      (ref.is_null (any.convert_extern (local.get 0))))
    (func (export "c_conv") (param externref) (result i32)
      try (result i32) (ref.is_null (any.convert_extern (local.get 0))) catch_all i32.const 0 end))"#;

#[test]
fn call_ref_null_and_bad_externref_are_typed() {
    compile_run_with(
        "typed_call_ref_null",
        NULL_FUNCREF_AND_EXTERN,
        typed_named(),
        &format!(
            r#"{CATCH}
            let mut inst = Instance::new();
            assert_eq!(inst.call_ok(), 7);
            let t = trap_of(|| inst.call_null());
            assert!(matches!(t, Wasm2RsTrap::NullReference));
            assert_eq!(t.to_string(), "null reference");
            assert!(matches!(trap_of(|| inst.call_param(u32::MAX)), Wasm2RsTrap::NullReference));
            assert!(matches!(trap_of(|| inst.tail_null()), Wasm2RsTrap::NullReference));
            assert!(matches!(trap_of(|| inst.c_null()), Wasm2RsTrap::NullReference));
            assert_eq!(inst.conv(u32::MAX), 1);
            let t = trap_of(|| inst.conv(5));
            assert!(matches!(t, Wasm2RsTrap::InvalidExternRef));
            assert_eq!(t.to_string(), "invalid externref");
            assert!(matches!(trap_of(|| inst.c_conv(5)), Wasm2RsTrap::InvalidExternRef));
            assert!(!HOOK_RAN.load(std::sync::atomic::Ordering::SeqCst));"#
        ),
    );
}

#[test]
fn call_ref_null_and_bad_externref_are_string_panics_when_untyped() {
    compile_run_with(
        "untyped_call_ref_null",
        NULL_FUNCREF_AND_EXTERN,
        TranspileOptions::default().with_named_exports(true),
        &format!(
            r#"{MSG_OF}
            let mut inst = Instance::new();
            assert_eq!(inst.call_ok(), 7);
            assert_eq!(msg_of(|| inst.call_null()), "null function reference");
            assert_eq!(msg_of(|| inst.call_param(u32::MAX)), "null function reference");
            assert_eq!(msg_of(|| inst.tail_null()), "null function reference");
            assert_eq!(inst.conv(u32::MAX), 1);
            assert_eq!(msg_of(|| inst.conv(5)), "invalid externref");
            assert_eq!(msg_of(|| inst.c_conv(5)), "invalid externref");"#
        ),
    );
}

#[test]
fn call_ref_null_check_uses_the_typed_trap_only_when_typed() {
    // Every `call_ref` gets the null check, even with no null funcref in sight; only
    // a typed build routes it to `Wasm2RsTrap`.
    let wat = r#"(module (type $ft (func))
        (func $f) (elem declare func $f)
        (func (export "go") (call_ref $ft (ref.func $f))))"#;
    let untyped = transpile_typed(wat, false);
    let typed = transpile_typed(wat, true);
    assert!(untyped.contains("null function reference"), "{untyped}");
    assert!(!untyped.contains("Wasm2RsTrap"), "{untyped}");
    assert!(typed.contains("trap_null_reference"), "{typed}");
    assert!(!typed.contains("InvalidExternRef"), "{typed}");
}

#[test]
fn a_trapping_argument_of_a_null_return_call_ref_traps_first() {
    // The arguments are evaluated before `return_call_ref` looks at the funcref, so
    // an out-of-bounds load among them wins over the null funcref.
    compile_run_with(
        "typed_return_call_ref_arg_first",
        r#"(module
            (type $ft (func (param i32) (result i32)))
            (memory 1)
            (func $f (param i32) (result i32) local.get 0)
            (elem declare func $f)
            (func (export "tail_oob") (param i32 i32) (result i32)
              (return_call_ref $ft (i32.load (local.get 1)) (ref.null $ft))))"#,
        typed_named(),
        &format!(
            r#"{CATCH}
            let mut inst = Instance::new();
            assert!(matches!(trap_of(|| inst.tail_oob(0, 1_000_000)), Wasm2RsTrap::OutOfBoundsMemoryAccess));
            assert!(matches!(trap_of(|| inst.tail_oob(0, 0)), Wasm2RsTrap::NullReference));"#
        ),
    );
}
