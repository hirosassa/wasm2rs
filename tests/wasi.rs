//! Integration tests for the native WASI subset (`proc_exit`, `fd_write`).
//!
//! These WASI functions are generated as inherent `Instance` methods that
//! read/write the module's linear memory directly, so a module whose only
//! imports are recognised WASI functions transpiles to a *standalone*
//! `Instance` with no injected host trait. The generated Rust is compiled with
//! `rustc -D warnings` and run; stdout and process exit codes are checked (no
//! mocking — a real `rustc` and a real child process).

use std::io::Write;
use std::process::{Command, Stdio};

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

// argv: `args_sizes_get(argc_ptr, buf_size_ptr)` then `args_get(argv_ptr,
// buf_ptr)`. `run` writes argc/buf_size at 0/4, the argv pointer table at 8 and
// the argument bytes at 64. The values come from the process's real argv.
const ARGS: &str = r#"
    (module
      (import "wasi_snapshot_preview1" "args_sizes_get"
        (func $sizes (param i32 i32) (result i32)))
      (import "wasi_snapshot_preview1" "args_get"
        (func $get (param i32 i32) (result i32)))
      (memory 1)
      (func (export "run")
        (drop (call $sizes (i32.const 0) (i32.const 4)))
        (drop (call $get (i32.const 8) (i32.const 64)))))
    "#;

#[test]
fn args_sizes_get_and_args_get_reflect_process_argv() {
    // `run` is func2 (imports occupy 0 and 1). The generated body checks the
    // written memory against `std::env::args()`, so it is self-verifying under
    // whatever argv the child is launched with.
    let extra = "\
fn main() {
    let mut i = Instance::new();
    i.func2();
    let expected: Vec<String> = std::env::args().collect();
    let argc = u32::from_le_bytes([i.mem()[0], i.mem()[1], i.mem()[2], i.mem()[3]]);
    assert_eq!(argc as usize, expected.len());
    let buf_size = u32::from_le_bytes([i.mem()[4], i.mem()[5], i.mem()[6], i.mem()[7]]);
    let want_size: usize = expected.iter().map(|s| s.len() + 1).sum();
    assert_eq!(buf_size as usize, want_size);
    for (k, arg) in expected.iter().enumerate() {
        let pe = 8 + k * 4;
        let ptr = u32::from_le_bytes([i.mem()[pe], i.mem()[pe + 1], i.mem()[pe + 2], i.mem()[pe + 3]]) as usize;
        assert_eq!(&i.mem()[ptr..ptr + arg.len()], arg.as_bytes());
        assert_eq!(i.mem()[ptr + arg.len()], 0);
    }
}
";
    let bin = compile("args", ARGS, extra);
    let status = Command::new(&bin)
        .args(["alpha", "beta"])
        .status()
        .expect("run generated binary");
    assert!(status.success(), "argv assertions failed");
}

// environ mirrors argv but the strings are `KEY=VALUE` from the environment.
const ENVIRON: &str = r#"
    (module
      (import "wasi_snapshot_preview1" "environ_sizes_get"
        (func $sizes (param i32 i32) (result i32)))
      (import "wasi_snapshot_preview1" "environ_get"
        (func $get (param i32 i32) (result i32)))
      (memory 1)
      (func (export "run")
        (drop (call $sizes (i32.const 0) (i32.const 4)))
        (drop (call $get (i32.const 8) (i32.const 64)))))
    "#;

#[test]
fn environ_sizes_get_and_environ_get_reflect_process_environ() {
    // Compare as a sorted set since environ order is unspecified.
    let extra = "\
fn main() {
    let mut i = Instance::new();
    i.func2();
    let mut expected: Vec<String> = std::env::vars().map(|(k, v)| format!(\"{k}={v}\")).collect();
    expected.sort();
    let count = u32::from_le_bytes([i.mem()[0], i.mem()[1], i.mem()[2], i.mem()[3]]) as usize;
    assert_eq!(count, expected.len());
    let mut got: Vec<String> = Vec::new();
    for k in 0..count {
        let pe = 8 + k * 4;
        let mut p = u32::from_le_bytes([i.mem()[pe], i.mem()[pe + 1], i.mem()[pe + 2], i.mem()[pe + 3]]) as usize;
        let mut bytes: Vec<u8> = Vec::new();
        while i.mem()[p] != 0 {
            bytes.push(i.mem()[p]);
            p += 1;
        }
        got.push(String::from_utf8(bytes).unwrap());
    }
    got.sort();
    assert_eq!(got, expected);
}
";
    let bin = compile("environ", ENVIRON, extra);
    // Clear the inherited environment for a deterministic, exact comparison.
    let status = Command::new(&bin)
        .env_clear()
        .env("FOO", "bar")
        .env("BAZ", "qux")
        .status()
        .expect("run generated binary");
    assert!(status.success(), "environ assertions failed");
}

// clock_time_get writes the current time in nanoseconds (u64) at the pointer.
const CLOCK: &str = r#"
    (module
      (import "wasi_snapshot_preview1" "clock_time_get"
        (func $now (param i32 i64 i32) (result i32)))
      (memory 1)
      (func (export "run")
        (drop (call $now (i32.const 0) (i64.const 0) (i32.const 0)))))
    "#;

#[test]
fn clock_time_get_writes_a_plausible_nanosecond_timestamp() {
    let extra = "\
fn main() {
    let mut i = Instance::new();
    i.func1();
    let ns = u64::from_le_bytes([
        i.mem()[0], i.mem()[1], i.mem()[2], i.mem()[3],
        i.mem()[4], i.mem()[5], i.mem()[6], i.mem()[7],
    ]);
    // After 2020-01-01 and before 2100 in nanoseconds since the Unix epoch.
    assert!(ns > 1_577_836_800_000_000_000, \"timestamp too small: {ns}\");
    assert!(ns < 4_102_444_800_000_000_000, \"timestamp too large: {ns}\");
}
";
    let bin = compile("clock", CLOCK, extra);
    let status = Command::new(&bin).status().expect("run generated binary");
    assert!(status.success(), "clock assertions failed");
}

// fd_read: read stdin into an iovec buffer. The iovec at offset 0 points at
// offset 16 with capacity 32; `run` reads fd 0 and returns the errno, with the
// byte count stored at offset 8.
const READ: &str = r#"
    (module
      (import "wasi_snapshot_preview1" "fd_read"
        (func $read (param i32 i32 i32 i32) (result i32)))
      (memory 1)
      (data (i32.const 0) "\10\00\00\00\20\00\00\00")
      (func (export "run") (result i32)
        (call $read (i32.const 0) (i32.const 0) (i32.const 1) (i32.const 8))))
    "#;

#[test]
fn fd_read_from_stdin_fills_iovec() {
    let extra = "\
fn main() {
    let mut i = Instance::new();
    let errno = i.func1();
    assert_eq!(errno, 0);
    let nread = u32::from_le_bytes([i.mem()[8], i.mem()[9], i.mem()[10], i.mem()[11]]) as usize;
    assert_eq!(&i.mem()[16..16 + nread], b\"hello wasi read\\n\");
}
";
    let bin = compile("read", READ, extra);
    let mut child = Command::new(&bin)
        .stdin(Stdio::piped())
        .spawn()
        .expect("spawn generated binary");
    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(b"hello wasi read\n")
        .expect("write stdin");
    let status = child.wait().expect("wait for child");
    assert!(status.success(), "fd_read assertions failed");
}

// random_get fills the buffer with random bytes from the OS.
const RANDOM: &str = r#"
    (module
      (import "wasi_snapshot_preview1" "random_get"
        (func $rand (param i32 i32) (result i32)))
      (memory 1)
      (func (export "run") (result i32)
        (call $rand (i32.const 0) (i32.const 16))))
    "#;

#[test]
fn random_get_fills_buffer_with_nonzero_entropy() {
    let extra = "\
fn main() {
    let mut i = Instance::new();
    assert_eq!(i.func1(), 0);
    // 16 zero bytes from a working RNG has probability ~2^-128.
    assert!(i.mem()[0..16].iter().any(|&b| b != 0), \"random bytes were all zero\");
}
";
    let bin = compile("random", RANDOM, extra);
    let status = Command::new(&bin).status().expect("run generated binary");
    assert!(status.success(), "random_get assertions failed");
}

// fd_fdstat_get: describe a file descriptor. `run` queries fd 1 into offset 0
// and returns the errno. A stdio fd is reported as a character device (2) with
// all rights granted.
const FDSTAT: &str = r#"
    (module
      (import "wasi_snapshot_preview1" "fd_fdstat_get"
        (func $fdstat (param i32 i32) (result i32)))
      (memory 1)
      (func (export "run") (result i32)
        (call $fdstat (i32.const 1) (i32.const 0))))
    "#;

#[test]
fn fd_fdstat_get_reports_stdio_as_a_character_device() {
    let extra = "\
fn main() {
    let mut i = Instance::new();
    assert_eq!(i.func1(), 0);
    assert_eq!(i.mem()[0], 2, \"stdio should be a character device\");
    let rights = u64::from_le_bytes([
        i.mem()[8], i.mem()[9], i.mem()[10], i.mem()[11],
        i.mem()[12], i.mem()[13], i.mem()[14], i.mem()[15],
    ]);
    assert_eq!(rights, u64::MAX, \"all base rights should be granted\");
}
";
    let bin = compile("fdstat", FDSTAT, extra);
    let status = Command::new(&bin).status().expect("run generated binary");
    assert!(status.success(), "fd_fdstat_get assertions failed");
}

// fd_fdstat_get on an unknown fd returns EBADF (8).
#[test]
fn fd_fdstat_get_rejects_unknown_fd() {
    let wat = r#"
        (module
          (import "wasi_snapshot_preview1" "fd_fdstat_get"
            (func $fdstat (param i32 i32) (result i32)))
          (memory 1)
          (func (export "run") (result i32)
            (call $fdstat (i32.const 7) (i32.const 0))))
        "#;
    let bin = compile(
        "fdstat_badf",
        wat,
        "fn main() {\n    let mut i = Instance::new();\n    assert_eq!(i.func1(), 8);\n}\n",
    );
    let status = Command::new(&bin).status().expect("run generated binary");
    assert!(status.success(), "expected EBADF for unknown fd");
}

// fd_close is a stub that reports success; the module needs no memory.
#[test]
fn fd_close_returns_success() {
    let wat = r#"
        (module
          (import "wasi_snapshot_preview1" "fd_close" (func $close (param i32) (result i32)))
          (func (export "run") (result i32) (call $close (i32.const 1))))
        "#;
    let generated = wasm2rs::transpile(&wat::parse_str(wat).expect("valid wat")).expect("ok");
    assert!(
        !generated.contains("trait Imports"),
        "should be standalone:\n{generated}"
    );
    let bin = compile(
        "close",
        wat,
        "fn main() {\n    let mut i = Instance::new();\n    assert_eq!(i.func1(), 0);\n}\n",
    );
    let status = Command::new(&bin).status().expect("run generated binary");
    assert!(status.success(), "fd_close should succeed");
}

// fd_seek on a stdio fd is not seekable, so it returns ESPIPE (70).
#[test]
fn fd_seek_on_stdio_returns_espipe() {
    let wat = r#"
        (module
          (import "wasi_snapshot_preview1" "fd_seek"
            (func $seek (param i32 i64 i32 i32) (result i32)))
          (func (export "run") (result i32)
            (call $seek (i32.const 1) (i64.const 0) (i32.const 0) (i32.const 0))))
        "#;
    let bin = compile(
        "seek",
        wat,
        "fn main() {\n    let mut i = Instance::new();\n    assert_eq!(i.func1(), 70);\n}\n",
    );
    let status = Command::new(&bin).status().expect("run generated binary");
    assert!(status.success(), "fd_seek should return ESPIPE");
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

// `sched_yield()` takes no arguments, touches no memory, and just returns
// success. A module importing only it transpiles to a standalone, memory-free
// `Instance`.
const YIELD: &str = r#"
    (module
      (import "wasi_snapshot_preview1" "sched_yield" (func $y (result i32)))
      (func (export "run") (result i32)
        call $y))
    "#;

#[test]
fn sched_yield_returns_success() {
    let generated =
        wasm2rs::transpile(&wat::parse_str(YIELD).expect("valid wat")).expect("transpile ok");
    assert!(
        generated.contains("fn wasi_sched_yield("),
        "sched_yield should be a native method:\n{generated}"
    );
    let bin = compile(
        "yield",
        YIELD,
        "fn main() {\n    let mut i = Instance::new();\n    assert_eq!(i.func1(), 0);\n}\n",
    );
    let status = Command::new(&bin).status().expect("run generated binary");
    assert!(status.success(), "sched_yield should return 0");
}

// `clock_res_get(clock_id, resolution_ptr)` writes the clock resolution (a u64
// nanosecond count) at the pointer and returns success. The value reported is
// 1 ns (the finest representable), regardless of clock id.
const CLOCK_RES: &str = r#"
    (module
      (import "wasi_snapshot_preview1" "clock_res_get"
        (func $res (param i32 i32) (result i32)))
      (memory 1)
      (func (export "run") (result i32)
        (call $res (i32.const 0) (i32.const 0))))
    "#;

#[test]
fn clock_res_get_writes_the_resolution() {
    let extra = "\
fn main() {
    let mut i = Instance::new();
    assert_eq!(i.func1(), 0);
    let res = u64::from_le_bytes([
        i.mem()[0], i.mem()[1], i.mem()[2], i.mem()[3],
        i.mem()[4], i.mem()[5], i.mem()[6], i.mem()[7],
    ]);
    assert_eq!(res, 1);
}
";
    let bin = compile("clock_res", CLOCK_RES, extra);
    let status = Command::new(&bin).status().expect("run generated binary");
    assert!(status.success(), "clock_res_get assertions failed");
}
