//! Spec-testsuite (`.wast`) harness.
//!
//! Each `module` directive and the assertions that follow it become one
//! generated Rust program (one `rustc` run): the module is transpiled and a
//! `main` replays the assertions against `Instance`. Directives the harness
//! cannot express yet are skipped *explicitly* and reported, never dropped.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code"
)]

use std::collections::HashMap;

use wast::core::{NanPattern, WastArgCore, WastRetCore};
use wast::parser::{self, ParseBuffer};
use wast::{QuoteWat, Wast, WastArg, WastDirective, WastExecute, WastInvoke, WastRet};

use crate::common;

/// What a `.wast` run did: how many assertions were executed and which
/// directives were skipped (each with the reason).
#[derive(Debug, Default)]
struct Report {
    executed: usize,
    skipped: Vec<String>,
}

impl Report {
    fn skip(&mut self, kind: &str, reason: impl std::fmt::Display) {
        self.skipped.push(format!("{kind}: {reason}"));
    }
}

/// The module currently being replayed: its transpilable bytes, the export
/// name -> absolute function index map, and the `main` body built so far.
struct Group {
    wasm: Vec<u8>,
    /// Stateful modules (memory, globals, ...) are an `Instance`; stateless
    /// ones are plain free functions.
    stateful: bool,
    funcs: HashMap<String, u32>,
    body: String,
}

/// Emitted at the top of every generated `main`: runs `f` and asserts that it
/// traps (panics), silencing the expected panic message while it runs.
const TRAP_HELPER: &str = "#[allow(dead_code)]
fn expect_trap<R>(f: impl FnOnce() -> R) {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    std::panic::set_hook(prev);
    assert!(r.is_err(), \"expected a trap\");
}
";

fn export_funcs(wasm: &[u8]) -> HashMap<String, u32> {
    let mut funcs = HashMap::new();
    for payload in wasmparser::Parser::new(0).parse_all(wasm) {
        if let wasmparser::Payload::ExportSection(reader) = payload.expect("valid wasm") {
            for export in reader {
                let export = export.expect("valid export");
                if export.kind == wasmparser::ExternalKind::Func {
                    funcs.insert(export.name.to_string(), export.index);
                }
            }
        }
    }
    funcs
}

/// The leading identifier of a value's `Debug` form, lowercased (`V128(..)` ->
/// `v128`): a short, stable label for a skip reason.
fn variant_name(value: &impl std::fmt::Debug) -> String {
    format!("{value:?}")
        .split(|c: char| !c.is_alphanumeric())
        .find(|part| !part.is_empty() && *part != "Core")
        .unwrap_or("value")
        .to_lowercase()
}

/// Rust expression for a core argument value.
fn arg_expr(arg: &WastArg<'_>) -> Result<String, String> {
    match arg {
        WastArg::Core(WastArgCore::I32(v)) => Ok(format!("{v}i32")),
        WastArg::Core(WastArgCore::I64(v)) => Ok(format!("{v}i64")),
        WastArg::Core(WastArgCore::F32(v)) => Ok(format!("f32::from_bits({}u32)", v.bits)),
        WastArg::Core(WastArgCore::F64(v)) => Ok(format!("f64::from_bits({}u64)", v.bits)),
        other => Err(format!("unsupported argument {}", variant_name(other))),
    }
}

/// Statement asserting that `call` (an expression) equals the expected result.
fn return_assertion(call: &str, ret: &WastRet<'_>) -> Result<String, String> {
    match ret {
        WastRet::Core(WastRetCore::I32(v)) => Ok(format!("assert_eq!({call}, {v}i32);")),
        WastRet::Core(WastRetCore::I64(v)) => Ok(format!("assert_eq!({call}, {v}i64);")),
        WastRet::Core(WastRetCore::F32(pattern)) => Ok(nan_assertion(
            call,
            "u32",
            "0x7fff_ffff",
            "0x7fc0_0000",
            "0x7fc0_0000",
            pattern,
        )),
        WastRet::Core(WastRetCore::F64(pattern)) => Ok(nan_assertion(
            call,
            "u64",
            "0x7fff_ffff_ffff_ffff",
            "0x7ff8_0000_0000_0000",
            "0x7ff8_0000_0000_0000",
            pattern,
        )),
        other => Err(format!("unsupported expected value {}", variant_name(other))),
    }
}

/// Assertion for a NaN pattern: *canonical* is exactly the quiet NaN with a
/// zero payload (either sign); *arithmetic* is any NaN whose quiet bit is set.
/// `Value` patterns are compared bit-for-bit by the caller instead.
fn nan_assertion<T: HasBits>(
    call: &str,
    bits_ty: &str,
    abs_mask: &str,
    canonical: &str,
    quiet: &str,
    pattern: &NanPattern<T>,
) -> String {
    match pattern {
        NanPattern::CanonicalNan => format!(
            "assert_eq!({call}.to_bits() & {abs_mask}{bits_ty}, {canonical}{bits_ty});"
        ),
        NanPattern::ArithmeticNan => format!(
            "assert_eq!({call}.to_bits() & {quiet}{bits_ty}, {quiet}{bits_ty});"
        ),
        NanPattern::Value(v) => format!("assert_eq!({call}.to_bits(), {}{bits_ty});", v.bits()),
    }
}

/// The raw bit pattern of a parsed float literal.
trait HasBits {
    fn bits(&self) -> u64;
}

impl HasBits for wast::token::F32 {
    fn bits(&self) -> u64 {
        u64::from(self.bits)
    }
}

impl HasBits for wast::token::F64 {
    fn bits(&self) -> u64 {
        self.bits
    }
}

/// Rust expression invoking `invoke` on the current module.
fn call_expr(group: &Group, invoke: &WastInvoke<'_>) -> Result<String, String> {
    if invoke.module.is_some() {
        return Err("invoke names another module instance".into());
    }
    let index = group
        .funcs
        .get(invoke.name)
        .ok_or_else(|| format!("no exported function {:?}", invoke.name))?;
    let args = invoke
        .args
        .iter()
        .map(arg_expr)
        .collect::<Result<Vec<_>, _>>()?;
    let receiver = if group.stateful { "inst." } else { "" };
    Ok(format!("{receiver}func{index}({})", args.join(", ")))
}

/// Statement for an `assert_return` on a single result.
fn return_stmt(
    group: &Group,
    invoke: &WastInvoke<'_>,
    results: &[WastRet<'_>],
) -> Result<String, String> {
    let call = call_expr(group, invoke)?;
    match results {
        [ret] => return_assertion(&call, ret),
        _ => Err("multi-value or empty result list".into()),
    }
}

/// Encode and vet one `module` directive into a [`Group`], or say why it
/// cannot be replayed.
fn start_group(module: &mut QuoteWat<'_>) -> Result<Group, String> {
    let wasm = module.encode().map_err(|e| format!("encode failed: {e}"))?;
    let mut has_imports = false;
    for payload in wasmparser::Parser::new(0).parse_all(&wasm) {
        if let Ok(wasmparser::Payload::ImportSection(_)) = payload {
            has_imports = true;
        }
    }
    if has_imports {
        return Err("module has imports".into());
    }
    let generated = wasm2rs::transpile(&wasm).map_err(|e| format!("transpile failed: {e:?}"))?;
    let stateful = generated.contains("pub struct Instance");
    let mut body = String::from(TRAP_HELPER);
    if stateful {
        body.push_str("let mut inst = Instance::new();\n");
    }
    Ok(Group {
        funcs: export_funcs(&wasm),
        wasm,
        stateful,
        body,
    })
}

fn run_wast(name: &str, src: &str) -> Report {
    let buf = ParseBuffer::new(src).expect("lex wast");
    let mut wast = parser::parse::<Wast>(&buf).expect("parse wast");
    let mut report = Report::default();
    let mut groups: Vec<Group> = Vec::new();
    // Whether the latest `module` was accepted (its group is `groups.last()`).
    let mut module_ok = false;
    for directive in &mut wast.directives {
        let (kind, outcome): (&str, Result<Option<String>, String>) = match directive {
            WastDirective::Module(module) => {
                match start_group(module) {
                    Ok(group) => {
                        groups.push(group);
                        module_ok = true;
                    }
                    Err(why) => {
                        module_ok = false;
                        report.skip("module", why);
                    }
                }
                continue;
            }
            WastDirective::AssertReturn {
                exec: WastExecute::Invoke(invoke),
                results,
                ..
            } => (
                "assert_return",
                with_group(&groups, module_ok, |g| return_stmt(g, invoke, results)),
            ),
            WastDirective::AssertTrap {
                exec: WastExecute::Invoke(invoke),
                ..
            } => (
                "assert_trap",
                with_group(&groups, module_ok, |g| {
                    call_expr(g, invoke).map(|c| format!("expect_trap(|| {{ let _ = {c}; }});"))
                }),
            ),
            WastDirective::Invoke(invoke) => (
                "invoke",
                with_group(&groups, module_ok, |g| {
                    call_expr(g, invoke).map(|c| format!("let _ = {c};"))
                }),
            ),
            WastDirective::AssertInvalid { .. } => ("assert_invalid", Err("not run".into())),
            WastDirective::AssertMalformed { .. } => ("assert_malformed", Err("not run".into())),
            WastDirective::AssertExhaustion { .. } => (
                "assert_exhaustion",
                Err("native stack overflow is not a trap".into()),
            ),
            WastDirective::Register { .. } => ("register", Err("multi-module linking".into())),
            other => (directive_name(other), Err("unsupported directive".into())),
        };
        match outcome {
            Ok(Some(stmt)) => {
                if let Some(group) = groups.last_mut() {
                    group.body.push_str(&stmt);
                    group.body.push('\n');
                }
                report.executed += 1;
            }
            Ok(None) => {}
            Err(why) => report.skip(kind, why),
        }
    }
    for (i, group) in groups.iter().enumerate() {
        common::compile_run_wasm(&format!("{name}_{i}"), &group.wasm, &group.body);
    }
    eprintln!(
        "{name}: {} executed, {} skipped",
        report.executed,
        report.skipped.len()
    );
    report
}

/// Run `f` against the current module's group, or report that it was skipped.
fn with_group(
    groups: &[Group],
    module_ok: bool,
    f: impl FnOnce(&Group) -> Result<String, String>,
) -> Result<Option<String>, String> {
    match groups.last() {
        Some(group) if module_ok => f(group).map(Some),
        _ => Err("its module was skipped".into()),
    }
}

/// `Debug` name of a directive without its payload, for skip reasons.
fn directive_name(directive: &WastDirective<'_>) -> &'static str {
    match directive {
        WastDirective::ModuleDefinition(_) => "module_definition",
        WastDirective::ModuleInstance { .. } => "module_instance",
        WastDirective::AssertUnlinkable { .. } => "assert_unlinkable",
        WastDirective::AssertException { .. } => "assert_exception",
        WastDirective::Thread(_) => "thread",
        _ => "directive",
    }
}

#[test]
fn assert_return_i32_add() {
    let report = run_wast(
        "spec_assert_return_i32_add",
        r#"
(module (func (export "add") (param i32 i32) (result i32)
  local.get 0 local.get 1 i32.add))
(assert_return (invoke "add" (i32.const 1) (i32.const 2)) (i32.const 3))
"#,
    );
    assert_eq!(report.executed, 1);
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
}

#[test]
fn assert_return_on_stateful_module() {
    let report = run_wast(
        "spec_assert_return_stateful",
        r#"
(module (memory 1) (global $g (mut i32) (i32.const 7))
  (func (export "load") (param i32) (result i32) local.get 0 i32.load)
  (func (export "g") (result i32) global.get $g))
(assert_return (invoke "load" (i32.const 0)) (i32.const 0))
(assert_return (invoke "g") (i32.const 7))
"#,
    );
    assert_eq!(report.executed, 2);
}

#[test]
fn assert_trap_passes_when_the_call_traps() {
    let report = run_wast(
        "spec_assert_trap_ok",
        r#"
(module (func (export "div") (param i32 i32) (result i32)
  local.get 0 local.get 1 i32.div_s))
(assert_trap (invoke "div" (i32.const 1) (i32.const 0)) "integer divide by zero")
(assert_return (invoke "div" (i32.const 6) (i32.const 3)) (i32.const 2))
"#,
    );
    assert_eq!(report.executed, 2);
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
}

#[test]
#[should_panic(expected = "generated program assertions failed")]
fn assert_trap_fails_when_the_call_does_not_trap() {
    run_wast(
        "spec_assert_trap_missing",
        r#"
(module (func (export "id") (param i32) (result i32) local.get 0))
(assert_trap (invoke "id" (i32.const 1)) "unreachable")
"#,
    );
}

#[test]
fn float_results_compare_by_bits_and_nan_patterns() {
    let report = run_wast(
        "spec_float_nan",
        r#"
(module
  (func (export "neg") (param f32) (result f32) local.get 0 f32.neg)
  (func (export "div") (param f64 f64) (result f64) local.get 0 local.get 1 f64.div)
  (func (export "id32") (param f32) (result f32) local.get 0))
;; -0.0 and +0.0 are different bit patterns
(assert_return (invoke "neg" (f32.const 0)) (f32.const -0))
;; a NaN with a payload is preserved bit-for-bit by a move
(assert_return (invoke "id32" (f32.const nan:0x200000)) (f32.const nan:0x200000))
;; 0/0 produces the canonical NaN; arithmetic NaN accepts any quiet NaN
(assert_return (invoke "div" (f64.const 0) (f64.const 0)) (f64.const nan:canonical))
(assert_return (invoke "div" (f64.const 0) (f64.const 0)) (f64.const nan:arithmetic))
(assert_return (invoke "id32" (f32.const nan:0x600000)) (f32.const nan:arithmetic))
"#,
    );
    assert_eq!(report.executed, 5);
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
}

#[test]
#[should_panic(expected = "generated program assertions failed")]
fn a_non_canonical_nan_does_not_satisfy_nan_canonical() {
    run_wast(
        "spec_float_nan_not_canonical",
        r#"
(module (func (export "id32") (param f32) (result f32) local.get 0))
(assert_return (invoke "id32" (f32.const nan:0x200000)) (f32.const nan:canonical))
"#,
    );
}

#[test]
fn unsupported_directives_are_skipped_with_a_reason() {
    let report = run_wast(
        "spec_skips",
        r#"
(module (func (export "id") (param i32) (result i32) local.get 0)
        (func (export "pair") (result i32 i32) i32.const 1 i32.const 2)
        (func (export "v") (param v128) (result v128) local.get 0))
(assert_return (invoke "id" (i32.const 5)) (i32.const 5))
(assert_return (invoke "pair") (i32.const 1) (i32.const 2))
(assert_return (invoke "v" (v128.const i32x4 1 2 3 4)) (v128.const i32x4 1 2 3 4))
(assert_invalid (module (func (result i32))) "type mismatch")
(assert_exhaustion (invoke "id" (i32.const 0)) "call stack exhausted")
(module $imp (import "spectest" "print_i32" (func (param i32)))
  (func (export "f") (result i32) i32.const 1))
(assert_return (invoke "f") (i32.const 1))
(register "imp" $imp)
"#,
    );
    // Only the first assertion is expressible.
    assert_eq!(report.executed, 1);
    let reasons = report.skipped.join("\n");
    for needle in [
        "multi-value",
        "v128",
        "assert_invalid",
        "assert_exhaustion",
        "imports",
        "register",
    ] {
        assert!(reasons.contains(needle), "no skip reason {needle:?} in:\n{reasons}");
    }
    // 2 unsupported assertions + invalid + exhaustion + import module + its
    // assertion + register.
    assert_eq!(report.skipped.len(), 7, "{reasons}");
}
