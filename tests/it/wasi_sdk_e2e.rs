//! End-to-end checks on binaries built by real toolchains (wasi-sdk's clang and
//! rustc's `wasm32-wasip1-threads` target), transpiled and run natively. They
//! need those tools, so each test skips with a message when its tool is
//! missing; see `docs/wasi-threads-e2e.md` for how to run them.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code"
)]

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::common::compile_run_wasm;
use wasmparser::{ExternalKind, Imports, Parser, Payload, TypeRef};

/// A fresh scratch directory for one test's sources and build output.
fn work_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wasm2rs_sdk_e2e_{name}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The `clang` of the wasi-sdk named by `WASI_SDK_PATH` (or `/opt/wasi-sdk`).
fn wasi_sdk_clang() -> Option<PathBuf> {
    let root = std::env::var_os("WASI_SDK_PATH")
        .map_or_else(|| PathBuf::from("/opt/wasi-sdk"), PathBuf::from);
    let clang = root.join("bin").join("clang");
    clang.is_file().then_some(clang)
}

/// The `emcc` of the emsdk named by `EMSDK` (set by `emsdk_env.sh`).
fn emcc() -> Option<PathBuf> {
    let emcc = Path::new(&std::env::var_os("EMSDK")?)
        .join("upstream")
        .join("emscripten")
        .join("emcc");
    emcc.is_file().then_some(emcc)
}

/// Whether rustc can target `wasm32-wasip1-threads` (its std is installed).
fn has_rust_wasi_threads_target() -> bool {
    let out = Command::new("rustc")
        .args([
            "--print",
            "target-libdir",
            "--target",
            "wasm32-wasip1-threads",
        ])
        .output()
        .expect("run rustc");
    out.status.success() && Path::new(String::from_utf8_lossy(&out.stdout).trim()).is_dir()
}

/// The function index of the export called `name`.
fn export_index(wasm: &[u8], name: &str) -> u32 {
    for payload in Parser::new(0).parse_all(wasm) {
        if let Payload::ExportSection(reader) = payload.unwrap() {
            for e in reader {
                let e = e.unwrap();
                if e.kind == ExternalKind::Func && e.name == name {
                    return e.index;
                }
            }
        }
    }
    panic!("no function export named {name}");
}

/// The function index of the imported function `module.name`.
fn import_index(wasm: &[u8], module: &str, name: &str) -> u32 {
    let mut index = 0;
    for payload in Parser::new(0).parse_all(wasm) {
        if let Payload::ImportSection(reader) = payload.unwrap() {
            for import in reader {
                if let Imports::Single(_, import) = import.unwrap()
                    && let TypeRef::Func(_) | TypeRef::FuncExact(_) = import.ty
                {
                    if import.module == module && import.name == name {
                        return index;
                    }
                    index += 1;
                }
            }
        }
    }
    panic!("no function import named {module}.{name}");
}

/// The initial size, in pages, of the imported memory (the host provides it).
fn imported_memory_pages(wasm: &[u8]) -> Option<u64> {
    for payload in Parser::new(0).parse_all(wasm) {
        if let Payload::ImportSection(reader) = payload.unwrap() {
            for import in reader {
                // Toolchains emit the plain (non-compact) import encoding.
                if let Imports::Single(_, import) = import.unwrap()
                    && let TypeRef::Memory(m) = import.ty
                {
                    return Some(m.initial);
                }
            }
        }
    }
    None
}

/// Whether `WASM2RS_REQUIRE_E2E` (its value is `flag`) makes `tool` mandatory:
/// `1` requires every tool, otherwise it is a comma-separated list of tool names
/// (`rust`, `wasi-sdk`, `emsdk`).
fn tool_required(flag: Option<&str>, tool: &str) -> bool {
    flag.is_some_and(|f| f.trim() == "1" || f.split(',').any(|t| t.trim() == tool))
}

/// Called when `tool` is missing: skip with a message, or fail when the tool is
/// required (so CI cannot go green without running the test).
fn skip_missing_tool(tool: &str, how_to_get_it: &str) {
    let flag = std::env::var("WASM2RS_REQUIRE_E2E").ok();
    assert!(
        !tool_required(flag.as_deref(), tool),
        "WASM2RS_REQUIRE_E2E requires {tool}, but it is missing: {how_to_get_it}"
    );
    eprintln!("skipped: {how_to_get_it}");
}

#[test]
fn the_require_flag_names_the_tools_that_must_not_be_skipped() {
    assert!(!tool_required(None, "rust"));
    assert!(!tool_required(Some(""), "rust"));
    assert!(tool_required(Some("1"), "rust"));
    assert!(tool_required(Some("1"), "wasi-sdk"));
    assert!(tool_required(Some("rust"), "rust"));
    assert!(!tool_required(Some("rust"), "wasi-sdk"));
    assert!(tool_required(Some("rust, wasi-sdk"), "wasi-sdk"));
    assert!(!tool_required(Some("rustc"), "rust"));
}

#[test]
fn a_wasi_sdk_pthread_reactor_runs_after_initialize() {
    let Some(clang) = wasi_sdk_clang() else {
        skip_missing_tool(
            "wasi-sdk",
            "set WASI_SDK_PATH to a wasi-sdk to run this test",
        );
        return;
    };
    let dir = work_dir("c_reactor");
    // The constructor sets `inited` (run by `_initialize`); `run` starts four
    // pthreads that each bump a shared atomic counter `n` times.
    std::fs::write(
        dir.join("reactor.c"),
        r#"
#include <pthread.h>
#include <stdatomic.h>
static atomic_int counter;
static int inited;
__attribute__((constructor)) static void ctor(void) { inited = 7; }
static void *work(void *arg) {
  for (int i = 0; i < (int)(long)arg; i++) atomic_fetch_add(&counter, 1);
  return 0;
}
__attribute__((export_name("run"))) int run(int n) {
  pthread_t t[4];
  atomic_store(&counter, 0);
  for (int i = 0; i < 4; i++) pthread_create(&t[i], 0, work, (void *)(long)n);
  for (int i = 0; i < 4; i++) pthread_join(t[i], 0);
  return atomic_load(&counter) + inited;
}
"#,
    )
    .unwrap();
    let status = Command::new(clang)
        .current_dir(&dir)
        .args([
            "--target=wasm32-wasip1-threads",
            "-pthread",
            "-mexec-model=reactor",
            "-O1",
            "reactor.c",
            "-o",
            "reactor.wasm",
            "-Wl,--max-memory=67108864",
        ])
        .status()
        .expect("run clang");
    assert!(status.success(), "clang failed");
    let wasm = std::fs::read(dir.join("reactor.wasm")).unwrap();

    let init = export_index(&wasm, "_initialize");
    let run = export_index(&wasm, "run");
    // 4 threads * n increments, plus `inited` (7) once `_initialize` ran; the
    // second call re-enters the same instance.
    compile_run_wasm(
        "sdk_c_reactor",
        &wasm,
        &format!(
            "let mut inst = Instance::new();\n\
             inst.func{init}();\n\
             assert_eq!(inst.func{run}(1000), 4007);\n\
             assert_eq!(inst.func{run}(500), 2007);"
        ),
    );
}

#[test]
fn a_rust_wasi_threads_command_counts_across_threads() {
    if !has_rust_wasi_threads_target() {
        skip_missing_tool(
            "rust",
            "run `rustup target add wasm32-wasip1-threads` to run this test",
        );
        return;
    }
    let dir = work_dir("rust_command");
    // The guest asserts its own result; a failed assertion traps `_start`.
    std::fs::write(
        dir.join("threads.rs"),
        r#"
use std::sync::{Arc, atomic::{AtomicU32, Ordering}};
fn main() {
    let c = Arc::new(AtomicU32::new(0));
    let hs: Vec<_> = (0..4).map(|_| {
        let c = c.clone();
        std::thread::spawn(move || { for _ in 0..1000 { c.fetch_add(1, Ordering::SeqCst); } })
    }).collect();
    for h in hs { h.join().unwrap(); }
    assert_eq!(c.load(Ordering::SeqCst), 4000);
}
"#,
    )
    .unwrap();
    let status = Command::new("rustc")
        .current_dir(&dir)
        .args([
            "--target",
            "wasm32-wasip1-threads",
            "-O",
            "threads.rs",
            "-o",
            "threads.wasm",
        ])
        .status()
        .expect("run rustc");
    assert!(status.success(), "rustc failed");
    let wasm = std::fs::read(dir.join("threads.wasm")).unwrap();

    let start = export_index(&wasm, "_start");
    let pages = imported_memory_pages(&wasm).expect("the module imports a shared memory");
    compile_run_wasm(
        "sdk_rust_command",
        &wasm,
        &format!(
            "let mut inst = Instance::new(SharedMemory::new({pages} * 65536));\n\
             inst.func{start}();"
        ),
    );
}

#[test]
fn a_wasi_sdk_pthread_command_counts_across_threads() {
    let Some(clang) = wasi_sdk_clang() else {
        skip_missing_tool(
            "wasi-sdk",
            "set WASI_SDK_PATH to a wasi-sdk to run this test",
        );
        return;
    };
    let dir = work_dir("c_command");
    // The command model: `_start` runs the constructors, then `main`. A wrong
    // count makes `main` return non-zero, which exits the process non-zero.
    std::fs::write(
        dir.join("command.c"),
        r#"
#include <pthread.h>
#include <stdatomic.h>
static atomic_int counter;
static void *work(void *arg) {
  for (int i = 0; i < 1000; i++) atomic_fetch_add(&counter, 1);
  return 0;
}
int main(void) {
  pthread_t t[4];
  for (int i = 0; i < 4; i++) pthread_create(&t[i], 0, work, 0);
  for (int i = 0; i < 4; i++) pthread_join(t[i], 0);
  return atomic_load(&counter) == 4000 ? 0 : 1;
}
"#,
    )
    .unwrap();
    let status = Command::new(clang)
        .current_dir(&dir)
        .args([
            "--target=wasm32-wasip1-threads",
            "-pthread",
            "-O1",
            "command.c",
            "-o",
            "command.wasm",
            "-Wl,--max-memory=67108864",
        ])
        .status()
        .expect("run clang");
    assert!(status.success(), "clang failed");
    let wasm = std::fs::read(dir.join("command.wasm")).unwrap();

    let start = export_index(&wasm, "_start");
    compile_run_wasm(
        "sdk_c_command",
        &wasm,
        &format!(
            "let mut inst = Instance::new();\n\
             inst.func{start}();"
        ),
    );
}

/// Build the four-thread atomic counter with emcc (`flags` pick the command or
/// JS form), transpile the `.wasm` and run it: `entry` gives the Rust that
/// drives the instance `inst`, and a wrong count makes it fail.
fn run_emscripten_counter(name: &str, flags: &[&str], entry: impl Fn(&[u8]) -> String) {
    let Some(emcc) = emcc() else {
        skip_missing_tool(
            "emsdk",
            "source an emsdk's emsdk_env.sh (sets EMSDK) to run this test",
        );
        return;
    };
    let dir = work_dir(name);
    // Emscripten starts a pthread by asking its JS host for a Worker
    // (`__pthread_create_js`); wasm2rs answers with a native thread.
    std::fs::write(
        dir.join("counter.c"),
        r#"
#include <pthread.h>
#include <stdatomic.h>
static atomic_int counter;
static void *work(void *arg) {
  for (int i = 0; i < 1000; i++) atomic_fetch_add(&counter, 1);
  return 0;
}
int main(void) {
  pthread_t t[4];
  for (int i = 0; i < 4; i++) pthread_create(&t[i], 0, work, 0);
  for (int i = 0; i < 4; i++) pthread_join(t[i], 0);
  return atomic_load(&counter) == 4000 ? 0 : 1;
}
"#,
    )
    .unwrap();
    let status = Command::new(emcc)
        .current_dir(&dir)
        .args(["-O1", "-pthread"])
        .args(flags)
        .args(["counter.c", "-o", "counter.js"])
        .status()
        .expect("run emcc");
    assert!(status.success(), "emcc failed");
    let wasm = std::fs::read(dir.join("counter.wasm")).unwrap();

    let pages = imported_memory_pages(&wasm).expect("imported memory");
    // The only imports left to the host proxy a call to the main thread, which
    // this program never does.
    let proxy = import_index(&wasm, "env", "_emscripten_receive_on_main_thread_js");
    compile_run_wasm(
        name,
        &wasm,
        &format!(
            "#[derive(Clone)]\n\
             struct Host;\n\
             impl Imports for Host {{\n\
                 fn import{proxy}(&mut self, _: i32, _: i32, _: i32, _: i32, _: i32, _: i32, _: i32) -> f64 {{\n\
                     panic!(\"unexpected proxy to the main thread\")\n\
                 }}\n\
             }}\n\
             let mut inst = Instance::new(SharedMemory::new({pages} * 65536), Host);\n{}",
            entry(&wasm)
        ),
    );
}

#[test]
fn an_emscripten_pthread_command_counts_across_threads() {
    run_emscripten_counter("emscripten_c_command", &["-sSTANDALONE_WASM"], |wasm| {
        format!("inst.func{}();", export_index(wasm, "_start"))
    });
}

#[test]
fn an_emscripten_js_build_counts_across_threads() {
    // The JS form has no `_start`: its runtime runs the constructors and then
    // `main(argc, argv)`, whose result is the exit status.
    run_emscripten_counter("emscripten_c_js", &[], |wasm| {
        format!(
            "inst.func{}();\nassert_eq!(inst.func{}(0, 0), 0);",
            export_index(wasm, "__wasm_call_ctors"),
            export_index(wasm, "main")
        )
    });
}
