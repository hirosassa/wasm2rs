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
    /// A state-changing call was skipped: the module's later assertions would see
    /// stale state, so they are skipped too.
    tainted: bool,
}

/// Emitted at the top of every generated `main`: runs `f`, asserts that it traps
/// (panics) and, when `needles` is non-empty, that the panic message contains one
/// of them. The expected panic is silenced while it runs.
const TRAP_HELPER: &str = "#[allow(dead_code)]
fn expect_trap<R>(ctx: &str, needles: &[&str], f: impl FnOnce() -> R) {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    std::panic::set_hook(prev);
    let payload = match r {
        Err(payload) => payload,
        Ok(_) => panic!(\"expected a trap ({})\", ctx),
    };
    let msg = payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_default();
    assert!(
        needles.is_empty() || needles.iter().any(|n| msg.contains(n)),
        \"trap {:?} is not one of {:?} ({})\",
        msg,
        needles,
        ctx
    );
}
";

/// Substrings of the generated code's panic message that correspond to a spec
/// trap message. Rust's own panics cover division and out-of-range memory, and
/// an out-of-range or null `call_indirect` slot reports a type mismatch, so those
/// are accepted. An unmapped message is only required to trap.
fn trap_needles(spec_message: &str) -> &'static [&'static str] {
    match spec_message {
        "integer divide by zero" => &["divide by zero", "divisor of zero"],
        "integer overflow" => &["overflow"],
        "invalid conversion to integer" => &["invalid conversion to integer"],
        "unreachable" => &["unreachable"],
        "out of bounds memory access" => &["out of range for slice", "out of bounds", "overflow"],
        "indirect call type mismatch" => &["indirect call type mismatch"],
        "undefined element" | "uninitialized element" => {
            &["indirect call type mismatch", "out of bounds"]
        }
        _ => &[],
    }
}

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

/// Whether a heap type is `func` or `extern`, the two reference types lowered to
/// a plain `u32` handle (a GC reference is a different Rust type).
fn is_func_or_extern(ty: &wast::core::HeapType<'_>) -> bool {
    use wast::core::AbstractHeapType;
    matches!(
        ty,
        wast::core::HeapType::Abstract {
            ty: AbstractHeapType::Func | AbstractHeapType::Extern,
            shared: false
        }
    )
}

/// Rust expression for a core argument value.
fn arg_expr(arg: &WastArg<'_>) -> Result<String, String> {
    match arg {
        WastArg::Core(WastArgCore::I32(v)) => Ok(format!("{v}i32")),
        WastArg::Core(WastArgCore::I64(v)) => Ok(format!("{v}i64")),
        WastArg::Core(WastArgCore::F32(v)) => Ok(format!("f32::from_bits({}u32)", v.bits)),
        WastArg::Core(WastArgCore::F64(v)) => Ok(format!("f64::from_bits({}u64)", v.bits)),
        // A `funcref`/`externref` is a `u32` handle and `u32::MAX` is null.
        WastArg::Core(WastArgCore::RefNull(ty)) if is_func_or_extern(ty) => {
            Ok("u32::MAX".to_string())
        }
        WastArg::Core(WastArgCore::RefExtern(n)) => Ok(format!("{n}u32")),
        other => Err(format!("unsupported argument {}", variant_name(other))),
    }
}

/// Statement asserting that `call` (an expression) equals the expected result.
fn return_assertion(call: &str, ret: &WastRet<'_>, ctx: &str) -> Result<String, String> {
    match ret {
        WastRet::Core(WastRetCore::I32(v)) => Ok(format!("assert_eq!({call}, {v}i32, \"{ctx}\");")),
        WastRet::Core(WastRetCore::I64(v)) => Ok(format!("assert_eq!({call}, {v}i64, \"{ctx}\");")),
        WastRet::Core(WastRetCore::F32(pattern)) => Ok(nan_assertion(
            call,
            ctx,
            "u32",
            "0x7fff_ffff",
            "0x7fc0_0000",
            "0x7fc0_0000",
            pattern,
        )),
        WastRet::Core(WastRetCore::F64(pattern)) => Ok(nan_assertion(
            call,
            ctx,
            "u64",
            "0x7fff_ffff_ffff_ffff",
            "0x7ff8_0000_0000_0000",
            "0x7ff8_0000_0000_0000",
            pattern,
        )),
        WastRet::Core(WastRetCore::RefNull(Some(ty))) if is_func_or_extern(ty) => {
            Ok(format!("assert_eq!({call}, u32::MAX, \"{ctx}\");"))
        }
        WastRet::Core(WastRetCore::RefExtern(Some(n))) => {
            Ok(format!("assert_eq!({call}, {n}u32, \"{ctx}\");"))
        }
        WastRet::Core(WastRetCore::RefExtern(None)) | WastRet::Core(WastRetCore::RefFunc(None)) => {
            Ok(format!("assert_ne!({call}, u32::MAX, \"{ctx}\");"))
        }
        WastRet::Core(WastRetCore::RefFunc(Some(wast::token::Index::Num(n, _)))) => {
            Ok(format!("assert_eq!({call}, {n}u32, \"{ctx}\");"))
        }
        other => Err(format!(
            "unsupported expected value {}",
            variant_name(other)
        )),
    }
}

/// Assertion for a NaN pattern: *canonical* is exactly the quiet NaN with a
/// zero payload (either sign); *arithmetic* is any NaN whose quiet bit is set.
/// `Value` patterns are compared bit-for-bit by the caller instead.
fn nan_assertion<T: HasBits>(
    call: &str,
    ctx: &str,
    bits_ty: &str,
    abs_mask: &str,
    canonical: &str,
    quiet: &str,
    pattern: &NanPattern<T>,
) -> String {
    match pattern {
        NanPattern::CanonicalNan => format!(
            "assert_eq!({call}.to_bits() & {abs_mask}{bits_ty}, {canonical}{bits_ty}, \"{ctx}\");"
        ),
        NanPattern::ArithmeticNan => {
            format!("assert_eq!({call}.to_bits() & {quiet}{bits_ty}, {quiet}{bits_ty}, \"{ctx}\");")
        }
        NanPattern::Value(v) => format!(
            "assert_eq!({call}.to_bits(), {}{bits_ty}, \"{ctx}\");",
            v.bits()
        ),
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
    ctx: &str,
) -> Result<String, String> {
    let call = call_expr(group, invoke)?;
    match results {
        [] => Ok(format!("let _ = {call};")),
        [ret] => return_assertion(&call, ret, ctx),
        _ => Err("multi-value result".into()),
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
        tainted: false,
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
        let ctx = directive_location(directive, src, name);
        // For an `assert_return` we cannot verify: still run the call, since
        // later assertions may observe its side effects.
        let mut effect_only: Option<String> = None;
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
                with_group(&groups, module_ok, |g| {
                    effect_only = call_expr(g, invoke).ok().map(|c| format!("let _ = {c};"));
                    return_stmt(g, invoke, results, &ctx)
                }),
            ),
            WastDirective::AssertTrap {
                exec: WastExecute::Invoke(invoke),
                message,
                ..
            } => (
                "assert_trap",
                with_group(&groups, module_ok, |g| {
                    call_expr(g, invoke).map(|c| {
                        let needles = format!("{:?}", trap_needles(message));
                        format!("expect_trap(\"{ctx}\", &{needles}, || {{ let _ = {c}; }});")
                    })
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
            Err(why) => {
                if let (Some(stmt), Some(group)) = (effect_only.as_ref(), groups.last_mut()) {
                    group.body.push_str(stmt);
                    group.body.push('\n');
                }
                // A call that could not be replayed may have changed the state the
                // module's later assertions observe.
                let state_call = matches!(kind, "invoke" | "assert_return" | "assert_trap");
                if state_call
                    && effect_only.is_none()
                    && module_ok
                    && why != SKIPPED_AFTER_TAINT
                    && let Some(group) = groups.last_mut()
                {
                    group.tainted = true;
                }
                report.skip(kind, why);
            }
        }
    }
    for (i, group) in groups.iter().enumerate() {
        run_group(&format!("{name}_{i}"), group);
    }
    eprintln!(
        "{name}: {} executed, {} skipped",
        report.executed,
        report.skipped.len()
    );
    let mut by_reason: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for reason in &report.skipped {
        *by_reason.entry(reason).or_default() += 1;
    }
    for (reason, count) in by_reason {
        eprintln!("  skipped x{count}: {reason}");
    }
    report
}

/// `<file>:<line>` of a directive, embedded in assertion messages so a failing
/// generated program points back at the `.wast` source.
fn directive_location(directive: &WastDirective<'_>, src: &str, name: &str) -> String {
    let (line, _) = directive.span().linecol_in(src);
    format!("{name}.wast:{}", line + 1)
}

/// Compile and run one group. A spec file calls only some of a module's
/// functions, so the harness (not the generator) tolerates unused code; every
/// other warning in the generated source is still an error.
fn run_group(name: &str, group: &Group) {
    let generated = wasm2rs::transpile(&group.wasm).expect("transpile module");
    let program = format!(
        "#![allow(dead_code, unused_mut, unused_variables)]\n{generated}\nfn main() {{\n{}\n}}\n",
        group.body
    );
    let bin = common::build(name, &program);
    let run = std::process::Command::new(&bin)
        .output()
        .expect("run generated binary");
    assert!(
        run.status.success(),
        "generated program assertions failed:\n{name}\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
}

const SKIPPED_AFTER_TAINT: &str = "an earlier call was skipped";

/// Run `f` against the current module's group, or report that it was skipped.
fn with_group(
    groups: &[Group],
    module_ok: bool,
    f: impl FnOnce(&Group) -> Result<String, String>,
) -> Result<Option<String>, String> {
    match groups.last() {
        Some(group) if module_ok && group.tainted => Err(SKIPPED_AFTER_TAINT.into()),
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
        assert!(
            reasons.contains(needle),
            "no skip reason {needle:?} in:\n{reasons}"
        );
    }
    // 2 unsupported assertions + invalid + exhaustion + import module + its
    // assertion + register.
    assert_eq!(report.skipped.len(), 7, "{reasons}");
}

/// A spec file that cannot be replayed yet because the transpiler rejects its
/// module. The test pins that reason: once the gap is fixed the assertion
/// fails and the entry must move to the plain list below.
#[allow(
    dead_code,
    reason = "no known gaps at the moment; kept to pin the next one"
)]
fn assert_known_gap(file: &str, report: &Report, reason: &str) {
    assert_eq!(
        report.executed, 0,
        "{file}: ran assertions despite a known gap"
    );
    assert!(
        report.skipped.iter().any(|s| s.contains(reason)),
        "{file}: known gap {reason:?} no longer reported (fixed? move it to the passing list):\n{:?}",
        report.skipped
    );
}

/// Replay a vendored `tests/spec/<file>.wast`.
macro_rules! spec_test {
    ($($name:ident => $file:literal),+ $(,)?) => {$(
        #[test]
        fn $name() {
            let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/spec/", $file, ".wast");
            let src = std::fs::read_to_string(path).expect("read vendored .wast");
            let report = run_wast(concat!("spec_", $file), &src);
            assert!(report.executed > 0, "{}: nothing was executed", $file);
        }
    )+};
}

/// Files whose module the transpiler rejects today (see `assert_known_gap`).
#[allow(
    unused_macros,
    reason = "no known gaps at the moment; kept to pin the next one"
)]
macro_rules! spec_known_gap {
    ($($name:ident => ($file:literal, $reason:literal)),+ $(,)?) => {$(
        #[test]
        fn $name() {
            let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/spec/", $file, ".wast");
            let src = std::fs::read_to_string(path).expect("read vendored .wast");
            let report = run_wast(concat!("spec_", $file), &src);
            assert_known_gap($file, &report, $reason);
        }
    )+};
}

spec_test! {
    spec_br_table => "br_table",
    spec_i32 => "i32",
    spec_i64 => "i64",
    spec_f32 => "f32",
    spec_f64 => "f64",
    spec_conversions => "conversions",
    spec_f32_cmp => "f32_cmp",
    spec_f64_cmp => "f64_cmp",
    spec_f32_bitwise => "f32_bitwise",
    spec_f64_bitwise => "f64_bitwise",
    spec_int_literals => "int_literals",
    spec_float_misc => "float_misc",
    spec_block => "block",
    spec_loop => "loop",
    spec_if_ => "if",
    spec_br => "br",
    spec_br_if => "br_if",
    spec_unwind => "unwind",
    spec_return => "return",
    spec_nop => "nop",
    spec_unreachable => "unreachable",
    spec_local_get => "local_get",
    spec_local_set => "local_set",
    spec_local_tee => "local_tee",
    spec_load => "load",
    spec_store => "store",
    spec_endianness => "endianness",
    spec_forward => "forward",
    spec_fac => "fac",
    spec_left_to_right => "left-to-right",
    spec_labels => "labels",
    spec_switch => "switch",
    spec_call => "call",
    spec_address => "address",
    spec_address64 => "address64",
    spec_align => "align",
    spec_align64 => "align64",
    spec_bulk => "bulk",
    spec_table_get => "table_get",
    spec_table_get64 => "table_get64",
    spec_table_fill => "table_fill",
    spec_table_fill64 => "table_fill64",
    spec_ref_is_null => "ref_is_null",
    spec_memory_init => "memory_init",
    spec_memory_init64 => "memory_init64",
    spec_bulk64 => "bulk64",
    spec_call_indirect => "call_indirect",
    spec_call_indirect64 => "call_indirect64",
    spec_const => "const",
    spec_func_ptrs => "func_ptrs",
    spec_memory => "memory",
    spec_memory64 => "memory64",
    spec_memory_multi => "memory-multi",
    spec_memory_copy => "memory_copy",
    spec_memory_copy64 => "memory_copy64",
    spec_memory_fill => "memory_fill",
    spec_memory_fill64 => "memory_fill64",
    spec_memory_grow => "memory_grow",
    spec_memory_redundancy => "memory_redundancy",
    spec_memory_redundancy64 => "memory_redundancy64",
    spec_memory_trap => "memory_trap",
    spec_memory_trap64 => "memory_trap64",
    spec_select => "select",
    spec_start => "start",
    spec_table_copy => "table_copy",
    spec_table_copy64 => "table_copy64",
    spec_table_init => "table_init",
    spec_table_init64 => "table_init64",
    spec_table_set => "table_set",
    spec_table_set64 => "table_set64",
    spec_traps => "traps",
}

#[test]
fn assert_trap_checks_the_trap_kind() {
    let report = run_wast(
        "spec_trap_kind_ok",
        r#"
(module
  (func (export "div") (param i32 i32) (result i32) local.get 0 local.get 1 i32.div_s)
  (func (export "ovf") (param i32 i32) (result i32) local.get 0 local.get 1 i32.div_s)
  (func (export "unr") unreachable))
(assert_trap (invoke "div" (i32.const 1) (i32.const 0)) "integer divide by zero")
(assert_trap (invoke "ovf" (i32.const 0x80000000) (i32.const -1)) "integer overflow")
(assert_trap (invoke "unr") "unreachable")
"#,
    );
    assert_eq!(report.executed, 3);
}

#[test]
#[should_panic(expected = "generated program assertions failed")]
fn assert_trap_fails_when_the_trap_kind_differs() {
    // The call traps, but with `unreachable`, not a division by zero.
    run_wast(
        "spec_trap_kind_wrong",
        r#"
(module (func (export "unr") unreachable))
(assert_trap (invoke "unr") "integer divide by zero")
"#,
    );
}

#[test]
fn an_unmapped_trap_message_only_requires_a_trap() {
    let report = run_wast(
        "spec_trap_kind_unmapped",
        r#"
(module (func (export "unr") unreachable))
(assert_trap (invoke "unr") "some message the harness has no mapping for")
"#,
    );
    assert_eq!(report.executed, 1);
}

#[test]
fn assertions_after_a_skipped_state_change_are_skipped_not_replayed() {
    // `set` takes a v128 argument the harness cannot express, so the call that
    // would set the global is skipped. Replaying the later assertions against the
    // stale state would fail for a reason that is no bug in the transpiler.
    let report = run_wast(
        "spec_taint",
        r#"
(module (global $g (mut i32) (i32.const 0))
  (func (export "set") (param v128) (global.set $g (i32.const 9)))
  (func (export "get") (result i32) global.get $g))
(invoke "set" (v128.const i32x4 0 0 0 0))
(assert_return (invoke "get") (i32.const 9))
"#,
    );
    assert_eq!(report.executed, 0);
    assert!(
        report
            .skipped
            .iter()
            .any(|s| s.contains("an earlier call was skipped")),
        "{:?}",
        report.skipped
    );
}

#[test]
fn a_skipped_call_does_not_taint_the_next_module() {
    let report = run_wast(
        "spec_taint_next_module",
        r#"
(module (func (export "set") (param v128)))
(invoke "set" (v128.const i32x4 0 0 0 0))
(module (func (export "one") (result i32) i32.const 1))
(assert_return (invoke "one") (i32.const 1))
"#,
    );
    assert_eq!(report.executed, 1);
}

const REF_MODULE: &str = r#"
(module (table $t 2 externref)
  (func $f)
  (elem declare func $f)
  (func (export "set") (param i32 externref) (table.set $t (local.get 0) (local.get 1)))
  (func (export "get") (param i32) (result externref) (table.get $t (local.get 0)))
  (func (export "isnull") (param externref) (result i32) (ref.is_null (local.get 0)))
  (func (export "fn") (result funcref) (ref.func $f)))
"#;

#[test]
fn reference_arguments_and_results_are_replayed() {
    let report = run_wast(
        "spec_refs",
        &format!(
            r#"{REF_MODULE}
(invoke "set" (i32.const 0) (ref.extern 7))
(assert_return (invoke "get" (i32.const 0)) (ref.extern 7))
(assert_return (invoke "get" (i32.const 0)) (ref.extern))
(assert_return (invoke "get" (i32.const 1)) (ref.null extern))
(assert_return (invoke "isnull" (ref.null extern)) (i32.const 1))
(assert_return (invoke "isnull" (ref.extern 3)) (i32.const 0))
(assert_return (invoke "fn") (ref.func))
(assert_return (invoke "fn") (ref.func 0))
"#
        ),
    );
    assert_eq!(report.executed, 8);
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
}

#[test]
#[should_panic(expected = "generated program assertions failed")]
fn a_wrong_reference_result_fails() {
    run_wast(
        "spec_refs_wrong",
        &format!(
            r#"{REF_MODULE}
(invoke "set" (i32.const 0) (ref.extern 7))
(assert_return (invoke "get" (i32.const 0)) (ref.extern 8))
"#
        ),
    );
}

#[test]
#[should_panic(expected = "generated program assertions failed")]
fn a_null_where_a_reference_is_expected_fails() {
    run_wast(
        "spec_refs_null_not_ref",
        &format!(
            r#"{REF_MODULE}
(assert_return (invoke "get" (i32.const 0)) (ref.extern))
"#
        ),
    );
}

#[test]
fn an_overflowing_bulk_range_counts_as_out_of_bounds() {
    // The destination is a "negative" i32, a huge address: the range arithmetic
    // overflows before the slice bounds check, which is still a trap.
    let report = run_wast(
        "spec_trap_overflow",
        r#"
(module (memory 1) (data $d "ab")
  (func (export "init") (param i32)
    (memory.init $d (local.get 0) (i32.const 0) (i32.const 2))))
(assert_trap (invoke "init" (i32.const -1)) "out of bounds memory access")
"#,
    );
    assert_eq!(report.executed, 1);
}
