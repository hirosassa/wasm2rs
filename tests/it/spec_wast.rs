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
use wast::{Wast, WastArg, WastDirective, WastExecute, WastInvoke, WastRet};

use crate::common;

/// What a `.wast` run did: how many assertions were executed and which
/// directives were skipped (each with the reason).
#[derive(Debug, Default)]
struct Report {
    executed: usize,
    skipped: Vec<String>,
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

/// Rust expression for a core argument value.
fn arg_expr(arg: &WastArg<'_>) -> Result<String, String> {
    match arg {
        WastArg::Core(WastArgCore::I32(v)) => Ok(format!("{v}i32")),
        WastArg::Core(WastArgCore::I64(v)) => Ok(format!("{v}i64")),
        WastArg::Core(WastArgCore::F32(v)) => Ok(format!("f32::from_bits({}u32)", v.bits)),
        WastArg::Core(WastArgCore::F64(v)) => Ok(format!("f64::from_bits({}u64)", v.bits)),
        other => Err(format!("unsupported argument {other:?}")),
    }
}

/// Statement asserting that `call` (an expression) equals the expected result.
fn return_assertion(call: &str, ret: &WastRet<'_>) -> Result<String, String> {
    match ret {
        WastRet::Core(WastRetCore::I32(v)) => Ok(format!("assert_eq!({call}, {v}i32);")),
        WastRet::Core(WastRetCore::I64(v)) => Ok(format!("assert_eq!({call}, {v}i64);")),
        WastRet::Core(WastRetCore::F32(NanPattern::Value(v))) => {
            Ok(format!("assert_eq!({call}.to_bits(), {}u32);", v.bits))
        }
        WastRet::Core(WastRetCore::F64(NanPattern::Value(v))) => {
            Ok(format!("assert_eq!({call}.to_bits(), {}u64);", v.bits))
        }
        other => Err(format!("unsupported expected value {other:?}")),
    }
}

/// Rust expression invoking `invoke` on the current module.
fn call_expr(group: &Group, invoke: &WastInvoke<'_>) -> String {
    let index = group.funcs[invoke.name];
    let args: Vec<String> = invoke
        .args
        .iter()
        .map(|a| arg_expr(a).expect("argument"))
        .collect();
    let receiver = if group.stateful { "inst." } else { "" };
    format!("{receiver}func{index}({})", args.join(", "))
}

fn run_wast(name: &str, src: &str) -> Report {
    let buf = ParseBuffer::new(src).expect("lex wast");
    let mut wast = parser::parse::<Wast>(&buf).expect("parse wast");
    let mut report = Report::default();
    let mut groups: Vec<Group> = Vec::new();
    for directive in &mut wast.directives {
        match directive {
            WastDirective::Module(module) => {
                let wasm = module.encode().expect("encode module");
                let generated = wasm2rs::transpile(&wasm).expect("transpile module");
                let stateful = generated.contains("pub struct Instance");
                let mut body = String::from(TRAP_HELPER);
                if stateful {
                    body.push_str("let mut inst = Instance::new();\n");
                }
                groups.push(Group {
                    funcs: export_funcs(&wasm),
                    wasm,
                    stateful,
                    body,
                });
            }
            WastDirective::AssertReturn {
                exec: WastExecute::Invoke(invoke),
                results,
                ..
            } => {
                let group = groups.last_mut().expect("assertion before any module");
                let call = call_expr(group, invoke);
                let stmt = match results.as_slice() {
                    [ret] => return_assertion(&call, ret).expect("result"),
                    _ => panic!("unsupported result arity"),
                };
                group.body.push_str(&stmt);
                group.body.push('\n');
                report.executed += 1;
            }
            WastDirective::AssertTrap {
                exec: WastExecute::Invoke(invoke),
                ..
            } => {
                let group = groups.last_mut().expect("assertion before any module");
                let call = call_expr(group, invoke);
                group
                    .body
                    .push_str(&format!("expect_trap(|| {{ let _ = {call}; }});\n"));
                report.executed += 1;
            }
            other => report.skipped.push(format!("{other:?}")),
        }
    }
    for (i, group) in groups.iter().enumerate() {
        common::compile_run_wasm(&format!("{name}_{i}"), &group.wasm, &group.body);
    }
    report
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
