//! Tests for the opt-in `TranspileOptions::typed_traps` (M6 stage 1).
//!
//! With the option on, the five runtime-helper traps (`unreachable`, indirect call
//! type mismatch, call stack exhausted and the two float->int conversion traps)
//! unwind with a `Wasm2RsTrap` payload (`resume_unwind`, so the panic hook never
//! runs) instead of a message string, the same mechanism a wasm exception uses.
//! Every other trap keeps its string payload, and the option is off by default.

use std::process::Command;
use wasm2rs::TranspileOptions;

use crate::common;

/// Functions 1-5 trap in the five helper ways; 6-10 call them under a `catch_all`,
/// which must not catch a trap; 11 is a trap outside stage 1 (a Rust `panic!`).
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
    let wasm = wat::parse_str(wat).expect("valid wat");
    wasm2rs::transpile_with_options(
        &wasm,
        &TranspileOptions::default()
            .with_typed_traps(typed)
            .with_call_depth_limit(Some(100)),
    )
    .expect("transpile ok")
}

fn compile_run_typed(name: &str, wat: &str, main_body: &str) {
    let program = format!(
        "{}\nfn main() {{\n{main_body}\n}}\n",
        transpile_typed(wat, true)
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
fn traps_outside_stage_one_keep_their_string_payload() {
    compile_run_typed(
        "typed_traps_other_panics",
        TRAPS,
        &format!(
            r#"{CATCH}
            let mut inst = Instance::new();
            let p = payload_of(|| inst.func11(0));
            assert!(p.downcast_ref::<Wasm2RsTrap>().is_none());
            assert!(p.downcast_ref::<&str>().is_some() || p.downcast_ref::<String>().is_some());"#
        ),
    );
}
