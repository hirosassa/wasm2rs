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

#[test]
fn a_wasi_sdk_pthread_reactor_runs_after_initialize() {
    let Some(clang) = wasi_sdk_clang() else {
        eprintln!("skipped: set WASI_SDK_PATH to a wasi-sdk to run this test");
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
        eprintln!("skipped: run `rustup target add wasm32-wasip1-threads` to run this test");
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
        eprintln!("skipped: set WASI_SDK_PATH to a wasi-sdk to run this test");
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
