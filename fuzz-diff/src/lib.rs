//! Differential fuzzing of wasm2rs against wasmtime.
//!
//! A module is run twice: by wasmtime (the reference) and as the Rust that wasm2rs
//! generates for it. Each exported function is called with fixed arguments, then
//! the linear memory is hashed. Both runs print the same line-per-event report,
//! and any difference between the two reports is a bug in one of them.

use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

use wasmparser::{ExternalKind, Parser, Payload, ValType};

/// Fuel for one exported call in wasmtime; a call that exhausts it makes the module
/// unusable for comparison (the Rust side has no fuel and could loop forever).
const FUEL: u64 = 2_000_000;

/// Argument values tried for integers, picked by call and parameter position.
const INTS: [i64; 10] = [
    0,
    1,
    -1,
    2,
    7,
    255,
    65_536,
    i32::MAX as i64,
    i32::MIN as i64,
    i64::MAX,
];
/// Bit patterns tried for floats (as `f64`; `f32` uses the matching `f32` bits).
const FLOATS: [f64; 8] = [0.0, -0.0, 1.0, -1.5, 3.0e9, f64::INFINITY, f64::NAN, 0.1];

/// An exported function the harness can call: numeric parameters and results only.
#[derive(Debug, Clone, PartialEq)]
pub struct Export {
    pub name: String,
    /// Absolute function index (there are no imports, so also the defined index).
    pub index: u32,
    pub params: Vec<ValType>,
    pub results: Vec<ValType>,
}

/// The result of comparing the two runs of one module.
#[derive(Debug, PartialEq)]
pub enum Verdict {
    Match,
    /// The module cannot be compared (reason given); not a finding.
    Skip(String),
    /// The reports differ.
    Mismatch {
        wasmtime: String,
        wasm2rs: String,
    },
    /// wasm2rs rejected the module (`Unsupported`) — worth a look, not a wrong result.
    Unsupported(String),
    /// wasm2rs produced code that does not compile (or crashed outright).
    Broken(String),
}

/// Compare two reports.
pub fn compare(wasmtime: &str, wasm2rs: &str) -> Verdict {
    if wasmtime == wasm2rs {
        Verdict::Match
    } else {
        Verdict::Mismatch {
            wasmtime: wasmtime.to_string(),
            wasm2rs: wasm2rs.to_string(),
        }
    }
}

fn numeric(t: ValType) -> bool {
    matches!(t, ValType::I32 | ValType::I64 | ValType::F32 | ValType::F64)
}

/// The module's exported functions that take and return only numbers, whether it
/// has an exported memory 0, and whether it imports anything (such modules are skipped).
pub fn inspect(wasm: &[u8]) -> Result<(Vec<Export>, bool, bool), String> {
    let mut types = Vec::new();
    let mut funcs = Vec::new();
    let mut exports = Vec::new();
    let (mut has_memory, mut imports) = (false, false);
    for payload in Parser::new(0).parse_all(wasm) {
        match payload.map_err(|e| e.to_string())? {
            Payload::TypeSection(r) => {
                for group in r {
                    for sub in group.map_err(|e| e.to_string())?.types() {
                        types.push(match &sub.composite_type.inner {
                            wasmparser::CompositeInnerType::Func(f) => {
                                Some((f.params().to_vec(), f.results().to_vec()))
                            }
                            _ => None,
                        });
                    }
                }
            }
            Payload::ImportSection(r) => imports = r.count() > 0,
            Payload::FunctionSection(r) => {
                for t in r {
                    funcs.push(t.map_err(|e| e.to_string())?);
                }
            }
            Payload::ExportSection(r) => {
                for e in r {
                    let e = e.map_err(|e| e.to_string())?;
                    match e.kind {
                        ExternalKind::Func => exports.push((e.name.to_string(), e.index)),
                        // Only memory 0 can be hashed on both sides, and only if exported.
                        ExternalKind::Memory if e.index == 0 => has_memory = true,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    for (name, index) in exports {
        let sig = funcs
            .get(index as usize)
            .and_then(|t| types.get(*t as usize))
            .and_then(Option::as_ref);
        if let Some((params, results)) = sig
            && params.iter().chain(results).all(|t| numeric(*t))
        {
            out.push(Export {
                name,
                index,
                params: params.clone(),
                results: results.clone(),
            });
        }
    }
    Ok((out, has_memory, imports))
}

/// The `n`th argument value for parameter `p` of call `c`, as raw bits.
fn arg_bits(c: usize, p: usize, t: ValType, round: usize) -> u64 {
    let pick = c
        .wrapping_mul(7)
        .wrapping_add(p.wrapping_mul(3))
        .wrapping_add(round * 5);
    match t {
        ValType::I32 => INTS[pick % INTS.len()] as i32 as u32 as u64,
        ValType::I64 => INTS[pick % INTS.len()] as u64,
        ValType::F32 => (FLOATS[pick % FLOATS.len()] as f32).to_bits() as u64,
        _ => FLOATS[pick % FLOATS.len()].to_bits(),
    }
}

/// The report line for one value: floats that are NaN print `nan` (payloads are
/// not deterministic across implementations).
fn fmt_val(t: ValType, bits: u64) -> String {
    match t {
        ValType::I32 => format!("i32:{}", bits as u32 as i32),
        ValType::I64 => format!("i64:{}", bits as i64),
        ValType::F32 if f32::from_bits(bits as u32).is_nan() => "f32:nan".into(),
        ValType::F32 => format!("f32:{:08x}", bits as u32),
        _ if f64::from_bits(bits).is_nan() => "f64:nan".into(),
        _ => format!("f64:{bits:016x}"),
    }
}

const ROUNDS: usize = 2;

/// How long the generated program may run before it counts as stuck.
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// Run `wasm` under wasmtime and build the report; `Err` means "cannot compare".
pub fn run_wasmtime(wasm: &[u8], exports: &[Export], has_memory: bool) -> Result<String, String> {
    use wasmtime::{Engine, Instance, Memory, Module, Store, Trap, Val};
    let mut cfg = wasmtime::Config::new();
    cfg.consume_fuel(true);
    let engine = Engine::new(&cfg).map_err(|e| e.to_string())?;
    let module = Module::new(&engine, wasm).map_err(|e| format!("wasmtime rejects: {e}"))?;
    let mut store = Store::new(&engine, ());
    let mut out = String::new();
    store.set_fuel(FUEL).map_err(|e| e.to_string())?;
    let instance = match Instance::new(&mut store, &module, &[]) {
        Ok(i) => i,
        Err(e) => {
            return match e.downcast_ref::<Trap>() {
                Some(Trap::OutOfFuel | Trap::StackOverflow) => Err(format!("init: {e}")),
                _ => Ok("init-trap\n".to_string()),
            };
        }
    };
    for round in 0..ROUNDS {
        for (c, ex) in exports.iter().enumerate() {
            let Some(func) = instance.get_func(&mut store, &ex.name) else {
                return Err(format!("no export {}", ex.name));
            };
            let args: Vec<Val> = ex
                .params
                .iter()
                .enumerate()
                .map(|(p, t)| {
                    let b = arg_bits(c, p, *t, round);
                    match t {
                        ValType::I32 => Val::I32(b as u32 as i32),
                        ValType::I64 => Val::I64(b as i64),
                        ValType::F32 => Val::F32(b as u32),
                        _ => Val::F64(b),
                    }
                })
                .collect();
            let mut results = vec![Val::I32(0); ex.results.len()];
            store.set_fuel(FUEL).map_err(|e| e.to_string())?;
            match func.call(&mut store, &args, &mut results) {
                Ok(()) => {
                    let vals: Vec<String> = ex
                        .results
                        .iter()
                        .zip(&results)
                        .map(|(t, v)| match v {
                            Val::I32(x) => fmt_val(*t, *x as u32 as u64),
                            Val::I64(x) => fmt_val(*t, *x as u64),
                            Val::F32(x) => fmt_val(*t, u64::from(*x)),
                            Val::F64(x) => fmt_val(*t, *x),
                            _ => "?".into(),
                        })
                        .collect();
                    let _ = writeln!(out, "{round}.{} ok {}", ex.index, vals.join(" "));
                }
                Err(e) => match e.downcast_ref::<Trap>() {
                    Some(Trap::OutOfFuel | Trap::StackOverflow) => {
                        return Err(format!("call {}: {e}", ex.name));
                    }
                    _ => {
                        let _ = writeln!(out, "{round}.{} trap", ex.index);
                    }
                },
            }
        }
    }
    if has_memory {
        let mem: Memory = find_memory(&mut store, &instance).ok_or("memory 0 is not exported")?;
        let _ = writeln!(out, "memory {}", fnv(mem.data(&store)));
    }
    Ok(out)
}

fn find_memory(
    store: &mut wasmtime::Store<()>,
    instance: &wasmtime::Instance,
) -> Option<wasmtime::Memory> {
    let names: Vec<String> = instance
        .exports(&mut *store)
        .map(|e| e.name().to_string())
        .collect();
    names
        .iter()
        .find_map(|n| instance.get_memory(&mut *store, n))
}

fn fnv(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h = (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3);
    }
    format!("{}:{h:016x}", bytes.len())
}

/// The generated Rust plus a `main` that prints the same report as
/// [`run_wasmtime`].
pub fn rust_program(generated: &str, exports: &[Export], has_memory: bool) -> String {
    let stateful = generated.contains("pub struct Instance");
    let mut body = String::new();
    for round in 0..ROUNDS {
        for (c, ex) in exports.iter().enumerate() {
            let args: Vec<String> = ex
                .params
                .iter()
                .enumerate()
                .map(|(p, t)| {
                    let b = arg_bits(c, p, *t, round);
                    match t {
                        ValType::I32 => format!("{}u32 as i32", b as u32),
                        ValType::I64 => format!("{b}u64 as i64"),
                        ValType::F32 => format!("f32::from_bits({})", b as u32),
                        _ => format!("f64::from_bits({b})"),
                    }
                })
                .collect();
            let call = if stateful {
                format!("inst.func{}({})", ex.index, args.join(", "))
            } else {
                format!("func{}({})", ex.index, args.join(", "))
            };
            let names: Vec<String> = (0..ex.results.len()).map(|i| format!("r{i}")).collect();
            let pat = match names.len() {
                0 => "_".to_string(),
                1 => names[0].clone(),
                _ => format!("({})", names.join(", ")),
            };
            let shown: Vec<String> = ex
                .results
                .iter()
                .zip(&names)
                .map(|(t, n)| match t {
                    ValType::I32 => format!("format!(\"i32:{{}}\", {n})"),
                    ValType::I64 => format!("format!(\"i64:{{}}\", {n})"),
                    ValType::F32 => format!(
                        "if {n}.is_nan() {{ \"f32:nan\".to_string() }} else {{ format!(\"f32:{{:08x}}\", {n}.to_bits()) }}"
                    ),
                    _ => format!(
                        "if {n}.is_nan() {{ \"f64:nan\".to_string() }} else {{ format!(\"f64:{{:016x}}\", {n}.to_bits()) }}"
                    ),
                })
                .collect();
            let _ = writeln!(
                body,
                "    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {call})) {{\n        \
                 Ok({pat}) => println!(\"{round}.{} ok {{}}\", [{}].iter().map(|s: &String| s.as_str()).collect::<Vec<&str>>().join(\" \")),\n        \
                 Err(_) => println!(\"{round}.{} trap\"),\n    }}",
                ex.index,
                shown.join(", "),
                ex.index
            );
        }
    }
    let new = if stateful {
        "    let mut inst = match std::panic::catch_unwind(Instance::new) {\n        Ok(i) => i,\n        Err(_) => { println!(\"init-trap\"); return; }\n    };\n"
    } else {
        ""
    };
    let mem = if has_memory && stateful {
        "    let mut h: u64 = 0xcbf29ce484222325;\n    let m = inst.memory();\n    for b in m.iter() { h = (h ^ u64::from(*b)).wrapping_mul(0x100000001b3); }\n    println!(\"memory {}:{:016x}\", m.len(), h);\n"
    } else {
        ""
    };
    format!(
        "{generated}\nfn run() {{\n{new}{body}{mem}}}\n\
         fn main() {{\n    std::panic::set_hook(Box::new(|_| {{}}));\n    \
         std::thread::Builder::new().stack_size(512 << 20).spawn(run).unwrap().join().unwrap();\n}}\n"
    )
}

/// Transpile, compile (in `work`) and run `wasm2rs`'s version, returning its report.
pub fn run_wasm2rs(
    wasm: &[u8],
    exports: &[Export],
    has_memory: bool,
    work: &Path,
) -> Result<String, Verdict> {
    let generated = match wasm2rs::transpile(wasm) {
        Ok(g) => g,
        Err(wasm2rs::TranspileError::Unsupported(m)) => return Err(Verdict::Unsupported(m)),
        Err(e) => return Err(Verdict::Broken(format!("transpile: {e}"))),
    };
    let program = rust_program(&generated, exports, has_memory);
    std::fs::create_dir_all(work).map_err(|e| Verdict::Skip(e.to_string()))?;
    let src = work.join("gen.rs");
    let bin = work.join("gen");
    std::fs::write(&src, &program).map_err(|e| Verdict::Skip(e.to_string()))?;
    let out = Command::new("rustc")
        .args(["--edition", "2024", "-C", "opt-level=0", "-A", "warnings"])
        .arg(&src)
        .arg("-o")
        .arg(&bin)
        .output()
        .map_err(|e| Verdict::Skip(e.to_string()))?;
    if !out.status.success() {
        return Err(Verdict::Broken(format!(
            "does not compile:\n{}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    // The generated program has no fuel; one that outlives wasmtime's run by this
    // much is stuck, which is a finding in its own right.
    let stdout_path = work.join("stdout");
    let mut child = Command::new(&bin)
        .stdout(std::fs::File::create(&stdout_path).map_err(|e| Verdict::Skip(e.to_string()))?)
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| Verdict::Skip(e.to_string()))?;
    let deadline = std::time::Instant::now() + TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() >= deadline => {
                child.kill().ok();
                child.wait().ok();
                return Err(Verdict::Broken(format!(
                    "timed out after {TIMEOUT:?} (wasmtime finished); stdout so far:\n{}",
                    std::fs::read_to_string(&stdout_path).unwrap_or_default()
                )));
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(20)),
            Err(e) => return Err(Verdict::Skip(e.to_string())),
        }
    };
    let stdout = std::fs::read_to_string(&stdout_path).unwrap_or_default();
    if !status.success() {
        let mut stderr = String::new();
        if let Some(mut e) = child.stderr.take() {
            std::io::Read::read_to_string(&mut e, &mut stderr).ok();
        }
        return Err(Verdict::Broken(format!(
            "crashed: {stderr}\nstdout so far:\n{stdout}"
        )));
    }
    Ok(stdout)
}

/// Run `wasm` through both implementations and compare. `work` is a scratch dir.
pub fn differential(wasm: &[u8], work: &Path) -> Verdict {
    let (exports, has_memory, imports) = match inspect(wasm) {
        Ok(x) => x,
        Err(e) => return Verdict::Skip(format!("parse: {e}")),
    };
    if imports {
        return Verdict::Skip("imports".into());
    }
    let reference = match run_wasmtime(wasm, &exports, has_memory) {
        Ok(r) => r,
        Err(e) => return Verdict::Skip(e),
    };
    match run_wasm2rs(wasm, &exports, has_memory, work) {
        Ok(actual) => compare(&reference, &actual),
        Err(v) => v,
    }
}

/// A deterministic module for `seed`: MVP plus multi-value, bulk memory, reference
/// types and the sign-extension / saturating-conversion operators, no imports,
/// small and without start-function surprises.
pub fn generate(seed: u64) -> Option<Vec<u8>> {
    use arbitrary::Unstructured;
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let bytes: Vec<u8> = (0..8192)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 24) as u8
        })
        .collect();
    let mut u = Unstructured::new(&bytes);
    let cfg = wasm_smith::Config {
        max_imports: 0,
        max_instructions: 150,
        max_funcs: 8,
        max_memory32_bytes: 4 * 65_536,
        max_table_elements: 100,
        export_everything: true,
        memory_max_size_required: true,
        table_max_size_required: true,
        simd_enabled: false,
        relaxed_simd_enabled: false,
        threads_enabled: false,
        exceptions_enabled: false,
        gc_enabled: false,
        memory64_enabled: false,
        tail_call_enabled: false,
        wide_arithmetic_enabled: false,
        extended_const_enabled: false,
        max_memories: 1,
        max_tables: 1,
        ..Default::default()
    };
    let module = wasm_smith::Module::new(cfg, &mut u).ok()?;
    Some(module.to_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("wasm2rs-fuzz-diff-{name}-{}", std::process::id()))
    }

    fn check(name: &str, wat: &str) -> Verdict {
        differential(&wat::parse_str(wat).unwrap(), &scratch(name))
    }

    #[test]
    fn compare_detects_a_difference() {
        assert_eq!(compare("0.0 ok i32:1\n", "0.0 ok i32:1\n"), Verdict::Match);
        assert!(matches!(
            compare("0.0 ok i32:1\n", "0.0 ok i32:2\n"),
            Verdict::Mismatch { .. }
        ));
        assert!(matches!(
            compare("0.0 ok i32:1\n", "0.0 trap\n"),
            Verdict::Mismatch { .. }
        ));
    }

    #[test]
    fn values_traps_and_memory_agree() {
        let v = check(
            "agree",
            r#"(module
              (memory (export "m") 1)
              (func (export "add") (param i32 i32) (result i32)
                (i32.store (i32.const 0) (local.get 0))
                (i32.add (local.get 0) (local.get 1)))
              (func (export "div") (param i32) (result i32)
                (i32.div_s (i32.const 1) (local.get 0)))
              (func (export "pair") (param f64) (result i64 f32)
                (i64.const 5) (f32.demote_f64 (local.get 0)))
              (func (export "boom") unreachable))"#,
        );
        assert_eq!(v, Verdict::Match);
    }

    #[test]
    fn the_report_shows_what_ran() {
        let wasm = wat::parse_str(
            r#"(module (func (export "id") (param i32) (result i32) (local.get 0))
                       (func (export "boom") unreachable))"#,
        )
        .unwrap();
        let (exports, mem, _) = inspect(&wasm).unwrap();
        let report = run_wasmtime(&wasm, &exports, mem).unwrap();
        assert!(report.contains("0.0 ok i32:0"), "{report}");
        assert!(report.contains("0.1 trap"), "{report}");
    }

    #[test]
    fn a_runaway_module_is_skipped_not_hung() {
        let v = check("loop", r#"(module (func (export "spin") (loop (br 0))))"#);
        assert!(matches!(v, Verdict::Skip(_)), "{v:?}");
    }

    #[test]
    fn a_start_trap_is_compared_too() {
        let v = check("start", r#"(module (func unreachable) (start 0))"#);
        assert_eq!(v, Verdict::Match);
    }

    /// Found by seed 162: a trapping load whose value is discarded by a function-level
    /// `br 0` is dropped by wasm2rs, so the trap never happens. Enable once fixed.
    #[test]
    #[ignore = "known wasm2rs bug: trapping load dropped before a function-level br"]
    fn a_trapping_load_before_a_function_level_br_still_traps() {
        let v = check(
            "br_load",
            r#"(module (memory (export "m") 0)
                 (func (export "f") (i32.load (i32.const 1)) (br 0)))"#,
        );
        assert_eq!(v, Verdict::Match);
    }

    /// Found by seeds 796 and 1080: `memory.grow` only enforces the 65536-page limit,
    /// not the memory's declared maximum, so growth that must fail (-1) succeeds.
    #[test]
    #[ignore = "known wasm2rs bug: memory.grow ignores the declared maximum"]
    fn memory_grow_respects_the_declared_maximum() {
        let v = check(
            "grow_max",
            r#"(module (memory (export "m") 1 2)
                 (func (export "grow") (param i32) (result i32) (memory.grow (local.get 0))))"#,
        );
        assert_eq!(v, Verdict::Match);
    }

    #[test]
    fn generated_modules_are_deterministic_and_valid() {
        let a = generate(1).unwrap();
        assert_eq!(a, generate(1).unwrap());
        assert!(wasmparser::validate(&a).is_ok());
    }
}
