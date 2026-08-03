//! Integration tests for the native WASI subset (`proc_exit`, `fd_write`).
//!
//! These WASI functions are generated as inherent `Instance` methods that
//! read/write the module's linear memory directly, so a module whose only
//! imports are recognised WASI functions transpiles to a *standalone*
//! `Instance` with no injected host trait. The generated Rust is compiled with
//! `rustc -D warnings` and run; stdout and process exit codes are checked (no
//! mocking — a real `rustc` and a real child process).

use std::process::Command;

/// Compile the transpiled module plus a trailing `extra` block (`fn main`).
fn compile(test: &str, wat: &str, extra: &str) -> std::path::PathBuf {
    let wasm = wat::parse_str(wat).expect("valid wat");
    let generated = wasm2rs::transpile(&wasm).expect("transpile ok");

    let dir = std::env::temp_dir().join(format!("wasm2rs_wasi_{test}"));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let src = dir.join("gen.rs");
    let bin = dir.join(if cfg!(windows) { "gen.exe" } else { "gen" });

    let program = format!("{generated}\n{extra}\n");
    std::fs::write(&src, &program).expect("write generated source");

    let out = Command::new("rustc")
        .current_dir(&dir)
        .arg(&src)
        .arg("--edition")
        .arg("2021")
        .arg("-D")
        .arg("warnings")
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("run rustc");
    assert!(
        out.status.success(),
        "generated code failed to compile:\n{program}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&out.stderr),
    );
    bin
}

// A module whose only import is `fd_write`. The linear memory holds one iovec at
// offset 0 (base = 16, len = 12) pointing at the string at offset 16. `run`
// writes it to fd 1 (stdout) and stores the byte count at offset 8.
const HELLO: &str = r#"
    (module
      (import "wasi_snapshot_preview1" "fd_write"
        (func $fd_write (param i32 i32 i32 i32) (result i32)))
      (memory 1)
      (data (i32.const 0) "\10\00\00\00\0c\00\00\00")
      (data (i32.const 16) "hello, wasi\n")
      (func (export "run")
        i32.const 1   ;; fd = stdout
        i32.const 0   ;; iovs pointer
        i32.const 1   ;; iovs count
        i32.const 8   ;; nwritten pointer
        call $fd_write
        drop))
    "#;

#[test]
fn fd_write_to_stdout_is_native_and_standalone() {
    let generated =
        wasm2rs::transpile(&wat::parse_str(HELLO).expect("valid wat")).expect("transpile ok");
    // A recognised-WASI-only module needs no host: no trait, no generic.
    assert!(
        !generated.contains("trait Imports"),
        "expected standalone output (no host trait):\n{generated}"
    );
    assert!(
        !generated.contains("Instance<H"),
        "expected standalone output (no generic host):\n{generated}"
    );

    // `run` is function index 1 (index 0 is the imported `fd_write`).
    let bin = compile(
        "hello",
        HELLO,
        "fn main() {\n    let mut i = Instance::new();\n    i.func1();\n}\n",
    );
    let out = Command::new(&bin).output().expect("run generated binary");
    assert!(out.status.success(), "binary exited with failure");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "hello, wasi\n");
}

// `proc_exit(code)` ends the process with the given code. The module has no
// memory, so it exercises the memory-free WASI path.
const EXIT: &str = r#"
    (module
      (import "wasi_snapshot_preview1" "proc_exit" (func $exit (param i32)))
      (func (export "run") (param i32)
        local.get 0
        call $exit))
    "#;

// A WASI import (index 0) alongside a non-WASI host import (index 1). The WASI
// one becomes a native method; the host trait must expose only `import1`,
// keeping the method number aligned to the absolute import index.
const MIXED: &str = r#"
    (module
      (import "wasi_snapshot_preview1" "fd_write"
        (func $fd_write (param i32 i32 i32 i32) (result i32)))
      (import "env" "log" (func $log (param i32)))
      (memory 1)
      (func (export "run") (param i32)
        local.get 0
        call $log))
    "#;

#[test]
fn wasi_and_host_imports_keep_trait_indices_aligned() {
    let generated =
        wasm2rs::transpile(&wat::parse_str(MIXED).expect("valid wat")).expect("transpile ok");
    // The host trait carries only the non-WASI import, at its absolute index.
    assert!(
        generated.contains("fn import1(&mut self, a0: i32);"),
        "host trait should expose import1:\n{generated}"
    );
    assert!(
        !generated.contains("fn import0("),
        "the WASI import must not appear in the host trait:\n{generated}"
    );
    assert!(
        generated.contains("fn wasi_fd_write("),
        "fd_write should be a native method:\n{generated}"
    );

    // `run` (func2) forwards its argument to the host `log` (import1); a Host
    // records it so we can prove the dispatch is wired to the right index.
    let extra = "\
struct Host { logged: i32 }
impl Imports for Host {
    fn import1(&mut self, a0: i32) { self.logged = a0; }
}
fn main() {
    let mut i = Instance::new(Host { logged: 0 });
    i.func2(7);
    assert_eq!(i.imports.logged, 7);
}
";
    let bin = compile("mixed", MIXED, extra);
    let status = Command::new(&bin).status().expect("run generated binary");
    assert!(status.success(), "host dispatch assertion failed");
}

// `fd_write` reads iovecs from linear memory, so a module importing it without
// declaring a memory cannot be honoured natively; transpile must reject it
// rather than emit code that references a non-existent `self.mem()`.
#[test]
fn fd_write_without_memory_is_rejected() {
    let wat = r#"
        (module
          (import "wasi_snapshot_preview1" "fd_write"
            (func $fd_write (param i32 i32 i32 i32) (result i32)))
          (func (export "run")
            i32.const 1
            i32.const 0
            i32.const 1
            i32.const 8
            call $fd_write
            drop))
        "#;
    let wasm = wat::parse_str(wat).expect("valid wat");
    let err = wasm2rs::transpile(&wasm).expect_err("should reject fd_write without memory");
    assert!(
        matches!(err, wasm2rs::TranspileError::Unsupported(_)),
        "expected Unsupported, got {err:?}"
    );
}

#[test]
fn proc_exit_terminates_with_code() {
    let bin = compile(
        "exit",
        EXIT,
        "fn main() {\n    let mut i = Instance::new();\n    i.func1(42);\n}\n",
    );
    let status = Command::new(&bin).status().expect("run generated binary");
    assert_eq!(status.code(), Some(42), "process should exit with code 42");
}
