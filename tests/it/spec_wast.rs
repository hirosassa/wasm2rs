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
    /// The expression building the instance (`None` for a stateless module).
    ctor: Option<String>,
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

#[allow(dead_code)]
fn expect_exception<R>(ctx: &str, f: impl FnOnce() -> R) {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    std::panic::set_hook(prev);
    let payload = match r {
        Err(payload) => payload,
        Ok(_) => panic!(\"expected an uncaught exception ({})\", ctx),
    };
    // A trap panics with a message; a thrown exception panics with its own payload.
    assert!(
        payload.downcast_ref::<&str>().is_none() && payload.downcast_ref::<String>().is_none(),
        \"expected an uncaught exception, got a trap ({})\",
        ctx
    );
}
";

/// Emitted after [`TRAP_HELPER`]: whether a reference result is null, for the
/// result types the harness cannot name (`u32` handles, and the managed `GcRef`
/// when the generated module defines it).
const NULL_REF_HELPER: &str = "#[allow(dead_code)]
trait NullRef { fn is_null_ref(&self) -> bool; }
impl NullRef for u32 { fn is_null_ref(&self) -> bool { *self == u32::MAX } }
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
        "call stack exhausted" => &["call stack exhausted"],
        // An overflowing `offset + len` bulk range panics in the index arithmetic.
        "out of bounds memory access" => &[
            "out of range for slice",
            "out of bounds",
            "add with overflow",
        ],
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
        // A `(ref.null)`, typed or not, is a null reference: the result is a `u32`
        // handle (`func`/`extern`/`exn` and the bottom types) or a `GcRef`, so ask the
        // `NullRef` helper.
        WastRet::Core(WastRetCore::RefNull(_)) => Ok(format!(
            "assert!(NullRef::is_null_ref(&{call}), \"{ctx}\");"
        )),
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

/// The host-owned state the `spectest` imports of one module stand for.
#[derive(Default)]
struct SpectestImports {
    /// Rust literal of each imported global's value, in import order (the
    /// `get_global{k}` index).
    globals: Vec<&'static str>,
    /// The module imports `spectest.table`.
    table: bool,
    /// The module imports `spectest.memory`.
    memory: bool,
}

impl SpectestImports {
    /// The `Host { .. }` expression matching [`spectest_host`]'s struct.
    fn host_init(&self) -> String {
        let table = if self.table { SPECTEST_TABLE_LEN } else { 0 };
        let memory = if self.memory {
            SPECTEST_MEMORY_PAGES * WASM_PAGE
        } else {
            0
        };
        format!("Host {{ table: vec![u32::MAX; {table}], memory: vec![0; {memory}] }}")
    }
}

/// The spec's `spectest` table is `(table 10 20 funcref)` and its memory is
/// `(memory 1 2)`. A module is linked against these actual sizes whatever minimum
/// it declares, and one they do not satisfy (a larger minimum, or a maximum below
/// the actual one) would not link, so it is not replayed.
const SPECTEST_TABLE_LEN: u64 = 10;
const SPECTEST_TABLE_MAX: u64 = 20;
const SPECTEST_MEMORY_PAGES: u64 = 1;
const SPECTEST_MEMORY_MAX: u64 = 2;
const WASM_PAGE: u64 = 65536;

/// The `spectest` import, if it is one the harness can replay: `print*` (no
/// result), the four `global_*` constants, `table` (funcref) and `memory`.
fn replay_spectest_import(
    import: &wasmparser::Import<'_>,
    no_result_types: &[bool],
    out: &mut SpectestImports,
) -> bool {
    use wasmparser::{TypeRef, ValType};
    if import.module != "spectest" {
        return false;
    }
    match import.ty {
        TypeRef::Func(t) => {
            import.name.starts_with("print")
                && no_result_types.get(t as usize).copied() == Some(true)
        }
        TypeRef::Global(g) if !g.mutable => {
            let value = match (import.name, g.content_type) {
                ("global_i32", ValType::I32) => "666",
                ("global_i64", ValType::I64) => "666",
                ("global_f32", ValType::F32) => "666.6",
                ("global_f64", ValType::F64) => "666.6",
                _ => return false,
            };
            out.globals.push(value);
            true
        }
        // A module importing the (single) spectest table twice is not replayable.
        TypeRef::Table(t)
            if import.name == "table"
                && !out.table
                && !t.table64
                && t.element_type.is_func_ref()
                && t.initial <= SPECTEST_TABLE_LEN
                && t.maximum.is_none_or(|max| max >= SPECTEST_TABLE_MAX) =>
        {
            out.table = true;
            true
        }
        TypeRef::Memory(m)
            if import.name == "memory"
                && !m.memory64
                && !m.shared
                && m.initial <= SPECTEST_MEMORY_PAGES
                && m.maximum.is_none_or(|max| max >= SPECTEST_MEMORY_MAX) =>
        {
            out.memory = true;
            true
        }
        _ => false,
    }
}

/// `struct Host` implementing the generated `Imports` trait: `spectest.print*` do
/// nothing, an imported global reads its `spectest` constant, and the table is a
/// host-owned vector of null slots. Each `fn name(..);` declaration of the trait
/// gets a body accordingly.
fn spectest_host(generated: &str, spectest: &SpectestImports) -> String {
    let start = generated
        .find("pub trait Imports {")
        .expect("Imports trait");
    let trait_body = &generated[start..];
    let end = trait_body.find("\n}").expect("end of Imports trait");
    let methods: String = trait_body[..end]
        .lines()
        .filter(|line| line.trim_end().ends_with(';'))
        .map(|line| {
            let decl = line.trim_end_matches(';');
            let body = if let Some(k) = decl
                .trim()
                .strip_prefix("fn get_global")
                .and_then(|r| r.split('(').next())
                .and_then(|k| k.parse::<usize>().ok())
            {
                spectest.globals[k]
            } else if decl.contains("fn table(") {
                "&self.table"
            } else if decl.contains("fn table_mut(") {
                "&mut self.table"
            } else if decl.contains("fn memory(") {
                "&self.memory"
            } else if decl.contains("fn memory_mut(") {
                "&mut self.memory"
            } else {
                ""
            };
            format!("{decl} {{ {body} }}\n")
        })
        .collect();
    format!(
        "struct Host {{ table: Vec<u32>, memory: Vec<u8> }}\n#[allow(unused_variables)]\nimpl Imports for Host {{\n{methods}}}\n"
    )
}

/// Whether a function body runs `table.grow` / `memory.grow` while the module
/// imports `spectest.table` / `spectest.memory`. The host is a plain vector, so the
/// real maximum (20 slots, 2 pages) is not enforced and the spec's failing grow
/// would not be reproduced.
fn grows_imported_spectest(wasm: &[u8], spectest: &SpectestImports) -> bool {
    use wasmparser::{Operator, Payload};
    wasmparser::Parser::new(0)
        .parse_all(wasm)
        .filter_map(Result::ok)
        .filter_map(|payload| match payload {
            Payload::CodeSectionEntry(body) => Some(body),
            _ => None,
        })
        .flat_map(|body| {
            body.get_operators_reader()
                .into_iter()
                .flatten()
                .filter_map(Result::ok)
                .collect::<Vec<_>>()
        })
        .any(|op| match op {
            Operator::TableGrow { .. } => spectest.table,
            Operator::MemoryGrow { .. } => spectest.memory,
            _ => false,
        })
}

fn encode_module(module: &mut QuoteWat<'_>) -> Result<Vec<u8>, String> {
    module.encode().map_err(|e| format!("encode failed: {e}"))
}

fn start_group(wasm: Vec<u8>) -> Result<Group, String> {
    // The spec's `spectest` host is replayed (see `replay_spectest_import`); an
    // import from any other module needs a real host and is skipped.
    let mut has_imports = false;
    let mut all_replayable = true;
    let mut spectest = SpectestImports::default();
    // Types whose result list is empty, by type index.
    let mut no_result_types: Vec<bool> = Vec::new();
    for payload in wasmparser::Parser::new(0).parse_all(&wasm) {
        match payload {
            Ok(wasmparser::Payload::TypeSection(reader)) => {
                for group in reader.into_iter().flatten() {
                    for ty in group.types() {
                        no_result_types.push(matches!(&ty.composite_type.inner,
                                wasmparser::CompositeInnerType::Func(f) if f.results().is_empty()));
                    }
                }
            }
            Ok(wasmparser::Payload::ImportSection(reader)) => {
                for import in reader.into_imports().flatten() {
                    has_imports = true;
                    all_replayable &=
                        replay_spectest_import(&import, &no_result_types, &mut spectest);
                }
            }
            _ => {}
        }
    }
    if has_imports && !all_replayable {
        return Err("module has imports".into());
    }
    if (spectest.table || spectest.memory) && grows_imported_spectest(&wasm, &spectest) {
        return Err("grows an imported spectest table or memory: its limit is not enforced".into());
    }
    let generated = transpile_module(&wasm).map_err(|e| format!("transpile failed: {e:?}"))?;
    let stateful = generated.contains("pub struct Instance");
    let mut body = String::from(TRAP_HELPER);
    body.push_str(NULL_REF_HELPER);
    if generated.contains("enum GcRef") {
        body.push_str(
            "impl NullRef for GcRef { fn is_null_ref(&self) -> bool { matches!(self, GcRef::Null) } }\n",
        );
    }
    let ctor = if has_imports {
        body.push_str(&spectest_host(&generated, &spectest));
        Some(format!("Instance::new({})", spectest.host_init()))
    } else if stateful {
        Some("Instance::new()".to_string())
    } else {
        None
    };
    if let Some(ctor) = &ctor {
        body.push_str(&format!("let mut inst = {ctor};\n"));
    }
    Ok(Group {
        funcs: export_funcs(&wasm),
        wasm,
        stateful,
        ctor,
        body,
        tainted: false,
    })
}

/// A group for `(assert_trap (module ...) msg)`: the generated `main` only checks
/// that building the instance traps. A module without an `Instance` (stateless) has
/// no instantiation to trap, so it is skipped.
/// Assert `transpile` rejects `module` (`what` is "invalid" or "malformed").
/// Validation happens in `transpile` itself, so no generated program is needed; a
/// module the text format cannot even encode is an `Err` (skipped).
fn assert_rejected(
    module: &mut QuoteWat<'_>,
    message: &str,
    what: &str,
    ctx: &str,
) -> Result<(), String> {
    let wasm = encode_module(module)?;
    assert!(
        matches!(
            wasm2rs::transpile(&wasm),
            Err(wasm2rs::TranspileError::Parse(_))
        ),
        "{ctx}: {what} module was not rejected by validation (expected {message:?})"
    );
    Ok(())
}

fn trapping_instantiation(wasm: Vec<u8>, message: &str, ctx: &str) -> Result<Group, String> {
    let mut group = start_group(wasm)?;
    let ctor = group
        .ctor
        .take()
        .ok_or("a stateless module has no instantiation to trap")?;
    // `start_group` ended the body with the construction; run it under
    // `expect_trap` instead.
    let at = group
        .body
        .rfind("let mut inst = ")
        .ok_or("no constructor to run")?;
    group.body.truncate(at);
    let needles = format!("{:?}", trap_needles(message));
    group.body.push_str(&format!(
        "expect_trap(\"{ctx}\", &{needles}, || {{ let _ = {ctor}; }});\n"
    ));
    Ok(group)
}

fn run_wast(name: &str, src: &str) -> Report {
    let buf = ParseBuffer::new(src).expect("lex wast");
    let mut wast = parser::parse::<Wast>(&buf).expect("parse wast");
    let mut report = Report::default();
    let mut groups: Vec<Group> = Vec::new();
    // Modules whose instantiation must trap. They never become the current module.
    let mut trap_groups: Vec<Group> = Vec::new();
    // Whether the latest `module` was accepted (its group is `groups.last()`).
    let mut module_ok = false;
    for directive in &mut wast.directives {
        let ctx = directive_location(directive, src, name);
        // For an `assert_return` we cannot verify: still run the call, since
        // later assertions may observe its side effects.
        let mut effect_only: Option<String> = None;
        let (kind, outcome): (&str, Result<Option<String>, String>) = match directive {
            WastDirective::Module(module) => {
                match encode_module(module).and_then(start_group) {
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
            WastDirective::AssertTrap {
                exec: WastExecute::Wat(wat),
                message,
                ..
            } => (
                // Not a `state_call`: the trapping instance has its own host and
                // cannot change the current module.
                "assert_trap_module",
                wat.encode()
                    .map_err(|e| format!("encode failed: {e}"))
                    .and_then(|wasm| trapping_instantiation(wasm, message, &ctx))
                    .map(|group| {
                        trap_groups.push(group);
                        None
                    }),
            ),
            WastDirective::AssertException {
                exec: WastExecute::Invoke(invoke),
                ..
            } => (
                "assert_exception",
                with_group(&groups, module_ok, |g| {
                    call_expr(g, invoke)
                        .map(|c| format!("expect_exception(\"{ctx}\", || {{ let _ = {c}; }});"))
                }),
            ),
            WastDirective::Invoke(invoke) => (
                "invoke",
                with_group(&groups, module_ok, |g| {
                    call_expr(g, invoke).map(|c| format!("let _ = {c};"))
                }),
            ),
            WastDirective::AssertInvalid {
                module, message, ..
            } => (
                "assert_invalid",
                assert_rejected(module, message, "invalid", &ctx).map(|()| {
                    report.executed += 1;
                    None
                }),
            ),
            // A malformed *text* module is rejected by the wast parser before it reaches
            // us (skipped); a malformed *binary* module must be rejected by `transpile`.
            WastDirective::AssertMalformed {
                module, message, ..
            } => (
                "assert_malformed",
                assert_rejected(module, message, "malformed", &ctx).map(|()| {
                    report.executed += 1;
                    None
                }),
            ),
            // Every module is transpiled with `SPEC_CALL_DEPTH_LIMIT`, so runaway
            // recursion traps with `call stack exhausted` instead of aborting.
            WastDirective::AssertExhaustion {
                call: invoke,
                message,
                ..
            } => (
                "assert_exhaustion",
                with_group(&groups, module_ok, |g| {
                    call_expr(g, invoke).map(|c| {
                        let needles = format!("{:?}", trap_needles(message));
                        format!("expect_trap(\"{ctx}\", &{needles}, || {{ let _ = {c}; }});")
                    })
                }),
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
    for (i, group) in trap_groups.iter().enumerate() {
        run_group(&format!("{name}_trap{i}"), group);
    }
    report.executed += trap_groups.len();
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

/// The call-depth limit every spec module is transpiled with, so that
/// `assert_exhaustion` can observe `call stack exhausted` as a trap. Deep enough for
/// the spec files' legitimate recursion, shallow enough for the 8 MiB main-thread
/// stack at opt-level 0.
const SPEC_CALL_DEPTH_LIMIT: u32 = 2_000;

fn transpile_module(wasm: &[u8]) -> Result<String, wasm2rs::TranspileError> {
    wasm2rs::transpile_with_options(
        wasm,
        &wasm2rs::TranspileOptions {
            call_depth_limit: Some(SPEC_CALL_DEPTH_LIMIT),
            ..Default::default()
        },
    )
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
    let generated = transpile_module(&group.wasm).expect("transpile module");
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
fn a_spectest_print_import_is_replayed_with_a_no_op_host() {
    let report = run_wast(
        "spec_spectest_print_import",
        r#"
(module
  (import "spectest" "print_i32_f32" (func $p (param i32 f32)))
  (func (export "f") (param i32) (result i32)
    (call $p (local.get 0) (f32.const 1))
    (i32.add (local.get 0) (i32.const 1))))
(assert_return (invoke "f" (i32.const 4)) (i32.const 5))
"#,
    );
    assert_eq!(report.executed, 1);
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
}

#[test]
fn a_module_level_assert_trap_expects_instantiation_to_trap() {
    // The data segment does not fit the 1-page memory, so instantiation traps; the
    // current module (`ok`) is untouched by it.
    let report = run_wast(
        "spec_assert_trap_module",
        r#"
(module (memory 1) (func (export "ok") (result i32) (i32.const 1)))
(assert_trap
  (module (memory 1) (data (i32.const 65536) "x"))
  "out of bounds memory access")
(assert_return (invoke "ok") (i32.const 1))
"#,
    );
    assert_eq!(report.executed, 2);
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
}

#[test]
#[should_panic(expected = "expected a trap")]
fn a_module_level_assert_trap_fails_when_instantiation_succeeds() {
    run_wast(
        "spec_assert_trap_module_no_trap",
        r#"
(assert_trap
  (module (memory 1) (data (i32.const 0) "x"))
  "out of bounds memory access")
"#,
    );
}

#[test]
fn spectest_globals_are_replayed_with_their_standard_values() {
    let report = run_wast(
        "spec_spectest_globals",
        r#"
(module
  (import "spectest" "global_i32" (global $a i32))
  (import "spectest" "global_i64" (global $b i64))
  (import "spectest" "global_f32" (global $c f32))
  (import "spectest" "global_f64" (global $d f64))
  (func (export "a") (result i32) (global.get $a))
  (func (export "b") (result i64) (global.get $b))
  (func (export "c") (result f32) (global.get $c))
  (func (export "d") (result f64) (global.get $d)))
(assert_return (invoke "a") (i32.const 666))
(assert_return (invoke "b") (i64.const 666))
(assert_return (invoke "c") (f32.const 666.6))
(assert_return (invoke "d") (f64.const 666.6))
"#,
    );
    assert_eq!(report.executed, 4);
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
}

#[test]
fn spectest_table_and_memory_have_their_actual_sizes_whatever_the_declared_minimum() {
    let report = run_wast(
        "spec_spectest_table_memory",
        r#"
(module
  (import "spectest" "table" (table 0 funcref))
  (import "spectest" "memory" (memory 0))
  (func (export "table_size") (result i32) (table.size 0))
  (func (export "memory_size") (result i32) (memory.size)))
(assert_return (invoke "table_size") (i32.const 10))
(assert_return (invoke "memory_size") (i32.const 1))
"#,
    );
    assert_eq!(report.executed, 2);
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
}

#[test]
fn a_spectest_import_the_real_host_would_not_link_is_skipped() {
    // The actual table is (10 20) and the memory (1 2): a larger minimum, or a
    // maximum below the actual one, does not link.
    for (i, import) in [
        r#"(import "spectest" "table" (table 11 funcref))"#,
        r#"(import "spectest" "table" (table 0 19 funcref))"#,
        r#"(import "spectest" "memory" (memory 2))"#,
        r#"(import "spectest" "memory" (memory 0 1))"#,
        // The host has one table: a second import of it is not replayable as a
        // separate host table.
        r#"(import "spectest" "table" (table 10 funcref)) (import "spectest" "table" (table 10 funcref))"#,
    ]
    .iter()
    .enumerate()
    {
        let report = run_wast(
            &format!("spec_spectest_unlinkable_{i}"),
            &format!(
                "(module {import} (func (export \"f\") (result i32) (i32.const 1)))\n(assert_return (invoke \"f\") (i32.const 1))"
            ),
        );
        assert_eq!(report.executed, 0, "{import}");
        assert!(
            report
                .skipped
                .iter()
                .any(|s| s.contains("module has imports")),
            "{import}: {:?}",
            report.skipped
        );
    }
}

#[test]
fn growing_an_imported_spectest_table_or_memory_is_skipped() {
    // The host is a plain vector, so the real maximum (20 slots, 2 pages) is not
    // enforced and the spec's `-1` would not be reproduced.
    for (i, wat) in [
        r#"(import "spectest" "table" (table 10 funcref))
           (func (export "g") (result i32) (table.grow (ref.null func) (i32.const 11)))"#,
        r#"(import "spectest" "memory" (memory 1))
           (func (export "g") (result i32) (memory.grow (i32.const 2)))"#,
    ]
    .iter()
    .enumerate()
    {
        let report = run_wast(
            &format!("spec_spectest_grow_{i}"),
            &format!("(module {wat})\n(assert_return (invoke \"g\") (i32.const -1))"),
        );
        assert_eq!(report.executed, 0, "{wat}");
        assert!(
            report.skipped.iter().any(|s| s.contains("limit")),
            "{wat}: {:?}",
            report.skipped
        );
    }
}

#[test]
fn a_skipped_module_level_assert_trap_does_not_taint_the_current_module() {
    let report = run_wast(
        "spec_skipped_trap_module_no_taint",
        r#"
(module (func (export "f") (result i32) i32.const 1))
(assert_trap
  (module (import "env" "x" (func)) (func unreachable) (start 1))
  "unreachable")
(assert_return (invoke "f") (i32.const 1))
"#,
    );
    assert_eq!(report.executed, 1);
    assert!(
        report.skipped.iter().any(|s| s.starts_with("assert_trap")),
        "{:?}",
        report.skipped
    );
}

#[test]
fn an_unknown_global_import_is_still_skipped() {
    let report = run_wast(
        "spec_unknown_global_import",
        r#"
(module
  (import "env" "g" (global i32))
  (func (export "f") (result i32) (global.get 0)))
(assert_return (invoke "f") (i32.const 0))
"#,
    );
    assert_eq!(report.executed, 0);
    assert!(
        report
            .skipped
            .iter()
            .any(|s| s.contains("module has imports")),
        "{:?}",
        report.skipped
    );
}

#[test]
fn an_untyped_ref_null_expectation_checks_for_null_whatever_the_reference_type() {
    let report = run_wast(
        "spec_untyped_ref_null",
        r#"
(module
  (func (export "func") (result funcref) (ref.null func))
  (func (export "any") (result anyref) (ref.null any))
  (func (export "nonnull") (result funcref) (ref.func 0)))
(assert_return (invoke "func") (ref.null))
(assert_return (invoke "any") (ref.null))
"#,
    );
    assert_eq!(report.executed, 2);
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
}

#[test]
#[should_panic(expected = "generated program assertions failed")]
fn an_untyped_ref_null_expectation_fails_for_a_non_null_reference() {
    run_wast(
        "spec_untyped_ref_null_fails",
        r#"
(module
  (func $f)
  (elem declare func $f)
  (func (export "nonnull") (result funcref) (ref.func $f)))
(assert_return (invoke "nonnull") (ref.null))
"#,
    );
}

#[test]
fn a_print_import_with_a_result_is_skipped() {
    let report = run_wast(
        "spec_print_with_result",
        r#"
(module
  (import "spectest" "print_ret" (func $p (result i32)))
  (func (export "f") (result i32) (call $p)))
(assert_return (invoke "f") (i32.const 0))
"#,
    );
    assert_eq!(report.executed, 0);
    assert!(
        report
            .skipped
            .iter()
            .any(|s| s.contains("module has imports")),
        "{:?}",
        report.skipped
    );
}

#[test]
fn a_non_print_import_is_still_skipped() {
    let report = run_wast(
        "spec_other_import_skipped",
        r#"
(module
  (import "env" "g" (func $g))
  (func (export "f") (call $g)))
(assert_return (invoke "f"))
"#,
    );
    assert_eq!(report.executed, 0);
    assert!(
        report
            .skipped
            .iter()
            .any(|s| s.contains("module has imports")),
        "{:?}",
        report.skipped
    );
}

#[test]
fn assert_exception_expects_an_uncaught_exception_not_a_trap() {
    let report = run_wast(
        "spec_assert_exception",
        r#"
(module
  (tag $e)
  (func (export "throws") (throw $e))
  (func (export "traps") (unreachable)))
(assert_exception (invoke "throws"))
"#,
    );
    assert_eq!(report.executed, 1);
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
}

#[test]
#[should_panic(expected = "expected an uncaught exception")]
fn assert_exception_fails_when_the_call_traps_instead() {
    run_wast(
        "spec_assert_exception_trap",
        r#"
(module
  (tag $e)
  (func (export "traps") (unreachable)))
(assert_exception (invoke "traps"))
"#,
    );
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
fn assert_exhaustion_runs_as_a_trap_under_the_call_depth_limit() {
    let report = run_wast(
        "spec_assert_exhaustion",
        r#"
(module
  (func $runaway (export "runaway") (call $runaway))
  (func $down (export "down") (param i32) (result i32)
    local.get 0
    if (result i32)
      local.get 0
      i32.const 1
      i32.sub
      call $down
    else
      i32.const 3
    end))
(assert_exhaustion (invoke "runaway") "call stack exhausted")
(assert_return (invoke "down" (i32.const 100)) (i32.const 3))
(assert_exhaustion (invoke "runaway") "call stack exhausted")
"#,
    );
    assert_eq!(report.executed, 3);
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
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
(module $imp (import "env" "g" (func (param i32)))
  (func (export "f") (result i32) i32.const 1))
(assert_return (invoke "f") (i32.const 1))
(register "imp" $imp)
"#,
    );
    // The first assertion and the `assert_invalid` (checked by validation) run.
    assert_eq!(report.executed, 2);
    let reasons = report.skipped.join("\n");
    for needle in ["multi-value", "v128", "imports", "register"] {
        assert!(
            reasons.contains(needle),
            "no skip reason {needle:?} in:\n{reasons}"
        );
    }
    // 2 unsupported assertions + import module + its assertion + register.
    assert_eq!(report.skipped.len(), 5, "{reasons}");
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
    spec_global => "global",
    spec_elem => "elem",
    spec_data => "data",
    spec_ref_null => "ref_null",
    spec_unreached_valid => "unreached-valid",
    spec_stack => "stack",
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
    spec_table_grow => "table_grow",
    spec_table_grow64 => "table_grow64",
    spec_table_size => "table_size",
    spec_table_size64 => "table_size64",
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
    spec_return_call => "return_call",
    spec_return_call_indirect => "return_call_indirect",
    spec_throw => "throw",
    spec_throw_ref => "throw_ref",
    spec_try_table => "try_table",
    spec_float_exprs => "float_exprs",
    spec_float_literals => "float_literals",
    spec_float_memory => "float_memory",
    spec_int_exprs => "int_exprs",
    spec_local_init => "local_init",
    spec_func => "func",
    spec_memory_size => "memory_size",
    spec_unreached_invalid => "unreached-invalid",
    spec_comments => "comments",
    spec_binary => "binary",
    spec_binary_leb128 => "binary-leb128",
    spec_custom => "custom",
    spec_exports => "exports",
    spec_call_ref => "call_ref",
    spec_return_call_ref => "return_call_ref",
    spec_br_on_null => "br_on_null",
    spec_br_on_non_null => "br_on_non_null",
    spec_ref_func => "ref_func",
    spec_imports => "imports",
    spec_table => "table",
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
