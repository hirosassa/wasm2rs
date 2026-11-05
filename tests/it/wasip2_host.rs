//! The minimal WASI preview2 host in `examples/wasip2_host/host.rs`, driven by a
//! small module that imports the same lowered functions a `wasm32-wasip2` hello
//! world does. The host is compiled together with the generated code; the module
//! writes to stdout through the canonical-ABI result layouts and exits.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::arithmetic_side_effects,
    clippy::float_cmp,
    clippy::lossy_float_literal,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::unwrap_in_result,
    reason = "test code"
)]

use std::process::Command;

const HOST: &str = include_str!("../../examples/wasip2_host/host.rs");

/// The 21 imports of a `println!("hello")` program, with their lowered types
/// (`(i32)`: resource handle or return pointer; `check-write` and `flush` take the
/// stream and a return pointer; `write` also a pointer and a length), plus a
/// `run` that checks the `check-write` and `write` result layouts and writes
/// "hi\n", and a `bye` that exits with an error status.
const HELLO: &str = r#"
(module
  (import "wasi:io/error@0.2.0" "[resource-drop]error" (func (param i32)))
  (import "wasi:io/poll@0.2.0" "[resource-drop]pollable" (func (param i32)))
  (import "wasi:io/streams@0.2.0" "[resource-drop]input-stream" (func (param i32)))
  (import "wasi:io/streams@0.2.0" "[resource-drop]output-stream" (func (param i32)))
  (import "wasi:cli/terminal-input@0.2.0" "[resource-drop]terminal-input" (func (param i32)))
  (import "wasi:cli/terminal-output@0.2.0" "[resource-drop]terminal-output" (func (param i32)))
  (import "wasi:cli/environment@0.2.0" "get-environment" (func $env (param i32)))
  (import "wasi:cli/exit@0.2.0" "exit" (func $exit (param i32)))
  (import "wasi:io/poll@0.2.0" "[method]pollable.block" (func $block (param i32)))
  (import "wasi:io/poll@0.2.0" "poll" (func $poll (param i32 i32 i32)))
  (import "wasi:io/streams@0.2.0" "[method]output-stream.check-write" (func $check (param i32 i32)))
  (import "wasi:io/streams@0.2.0" "[method]output-stream.write" (func $write (param i32 i32 i32 i32)))
  (import "wasi:io/streams@0.2.0" "[method]output-stream.blocking-flush" (func $flush (param i32 i32)))
  (import "wasi:io/streams@0.2.0" "[method]output-stream.subscribe" (func (param i32) (result i32)))
  (import "wasi:cli/stdin@0.2.0" "get-stdin" (func (result i32)))
  (import "wasi:cli/stdout@0.2.0" "get-stdout" (func $stdout (result i32)))
  (import "wasi:cli/stderr@0.2.0" "get-stderr" (func $stderr (result i32)))
  (import "wasi:cli/terminal-stdin@0.2.0" "get-terminal-stdin" (func (param i32)))
  (import "wasi:cli/terminal-stdout@0.2.0" "get-terminal-stdout" (func $term (param i32)))
  (import "wasi:cli/terminal-stderr@0.2.0" "get-terminal-stderr" (func (param i32)))
  (import "wasi:clocks/monotonic-clock@0.2.0" "subscribe-duration" (func $sub_dur (param i64) (result i32)))
  (memory (export "memory") 1)
  (global $brk (mut i32) (i32.const 1024))
  ;; a bump allocator: the host reaches it as `cabi_realloc` (old pointer and size
  ;; are 0 for every allocation a host makes)
  (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32) (local $p i32)
    (local.set $p (i32.and (i32.add (global.get $brk) (i32.sub (local.get 2) (i32.const 1)))
                           (i32.sub (i32.const 0) (local.get 2))))
    (global.set $brk (i32.add (local.get $p) (local.get 3)))
    (local.get $p))
  (data (i32.const 64) "hi\n")
  (data (i32.const 67) "e\n")
  (func (export "run") (local $out i32)
    (local.set $out (call $stdout))
    ;; result<u64, stream-error> at 16: ok tag at 16, the budget at 24
    (call $check (local.get $out) (i32.const 16))
    (if (i32.load8_u (i32.const 16)) (then unreachable))
    (if (i64.ne (i64.load (i32.const 24)) (i64.const 4096)) (then unreachable))
    ;; result<_, stream-error> at 32
    (i32.store8 (i32.const 32) (i32.const 9))
    (call $write (local.get $out) (i32.const 64) (i32.const 3) (i32.const 32))
    (if (i32.load8_u (i32.const 32)) (then unreachable))
    (i32.store8 (i32.const 40) (i32.const 9))
    (call $flush (local.get $out) (i32.const 40))
    (if (i32.load8_u (i32.const 40)) (then unreachable))
    ;; an empty list<_> at 48 is (ptr, len): the length, at 52, is 0
    (i64.store (i32.const 48) (i64.const -1))
    (call $env (i32.const 48))
    (if (i32.load (i32.const 52)) (then unreachable))
    ;; option<terminal> none at 56
    (i32.store8 (i32.const 56) (i32.const 9))
    (call $term (i32.const 56))
    (if (i32.load8_u (i32.const 56)) (then unreachable))
    ;; get-stderr is a different stream from stdout
    (call $write (call $stderr) (i32.const 67) (i32.const 2) (i32.const 32)))
  (func (export "bye") (call $exit (i32.const 1))))
"#;

include!("../../examples/wasip2_host/filter.rs");

fn compile(test: &str, wat: &str, host_init: &str, body: &str) -> std::path::PathBuf {
    let wasm = wat::parse_str(wat).expect("valid wat");
    let opts = wasm2rs::TranspileOptions {
        named_exports: true,
        named_imports: true,
        reentrant_imports: true,
        ..wasm2rs::TranspileOptions::default()
    };
    let generated = wasm2rs::transpile_with_options(&wasm, &opts).expect("transpile ok");

    let dir = std::env::temp_dir().join(format!("wasm2rs_wasip2_host_{test}"));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let src = dir.join("gen.rs");
    let bin = dir.join(if cfg!(windows) { "gen.exe" } else { "gen" });
    let main =
        format!("fn main() {{\n    let mut inst = Instance::new({host_init});\n    {body}\n}}");
    let host = keep_declared_methods(HOST, &generated);
    std::fs::write(&src, format!("{generated}\n{host}\n{main}\n")).expect("write source");

    let out = Command::new("rustc")
        .current_dir(&dir)
        .arg(&src)
        .arg("--edition")
        .arg("2024")
        .arg("-D")
        .arg("warnings")
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("run rustc");
    assert!(
        out.status.success(),
        "generated code failed to compile:\n--- stderr ---\n{}",
        String::from_utf8_lossy(&out.stderr),
    );
    bin
}

#[test]
fn a_preview2_module_writes_to_stdout_and_exits_through_the_host() {
    let host = "P2Host::new(vec![], vec![])";
    let bin = compile("hello", HELLO, host, "inst.run();\n    inst.bye();");
    let run = Command::new(&bin).output().expect("run generated binary");
    assert_eq!(String::from_utf8_lossy(&run.stdout), "hi\n");
    assert_eq!(String::from_utf8_lossy(&run.stderr), "e\n");
    assert_eq!(run.status.code(), Some(1), "exit(error) should exit with 1");
}

/// `args` checks, through the canonical-ABI layouts, that the host built the
/// argument and environment lists in the guest's memory with `cabi_realloc`
/// (`list<string>`: 8-byte `(ptr, len)` elements; `list<tuple<string, string>>`:
/// 16-byte ones) and that `poll` returns the indices of the ready pollables.
const ARGS: &str = r#"
  (func (export "args") (local $p i32)
    (i64.store (i32.const 72) (i64.const -1))
    (call $args (i32.const 72))
    (if (i32.ne (i32.load (i32.const 76)) (i32.const 2)) (then unreachable))
    (local.set $p (i32.load (i32.const 72)))
    (if (i32.ne (i32.load offset=4 (local.get $p)) (i32.const 4)) (then unreachable))
    (if (i32.ne (i32.load (i32.load (local.get $p))) (i32.const 0x676f7270)) (then unreachable))
    (if (i32.ne (i32.load offset=12 (local.get $p)) (i32.const 1)) (then unreachable))
    (if (i32.ne (i32.load8_u (i32.load offset=8 (local.get $p))) (i32.const 120)) (then unreachable))
    (i64.store (i32.const 80) (i64.const -1))
    (call $env (i32.const 80))
    (if (i32.ne (i32.load (i32.const 84)) (i32.const 1)) (then unreachable))
    (local.set $p (i32.load (i32.const 80)))
    (if (i32.ne (i32.load offset=4 (local.get $p)) (i32.const 1)) (then unreachable))
    (if (i32.ne (i32.load8_u (i32.load (local.get $p))) (i32.const 75)) (then unreachable))
    (if (i32.ne (i32.load offset=12 (local.get $p)) (i32.const 2)) (then unreachable))
    (if (i32.ne (i32.load16_u (i32.load offset=8 (local.get $p))) (i32.const 0x5856)) (then unreachable))
    (i32.store (i32.const 96) (call $sub_dur (i64.const 0)))
    (i32.store (i32.const 100) (call $sub_dur (i64.const 0)))
    (i64.store (i32.const 104) (i64.const -1))
    (call $poll (i32.const 96) (i32.const 2) (i32.const 104))
    (if (i32.ne (i32.load (i32.const 108)) (i32.const 2)) (then unreachable))
    (local.set $p (i32.load (i32.const 104)))
    (if (i32.ne (i32.load (local.get $p)) (i32.const 0)) (then unreachable))
    (if (i32.ne (i32.load offset=4 (local.get $p)) (i32.const 1)) (then unreachable)))
"#;

#[test]
fn a_host_builds_arguments_environment_and_poll_results_in_guest_memory() {
    // A std program also imports `get-arguments`; the host provides it only then.
    let wat = hello_with(
        "(import \"wasi:cli/environment@0.2.0\" \"get-arguments\" (func $args (param i32)))",
        ARGS,
    );
    let host = r#"P2Host::new(vec!["prog".to_string(), "x".to_string()], vec![("K".to_string(), "VX".to_string())])"#;
    let bin = compile("with_arguments", &wat, host, "inst.args();");
    let run = Command::new(&bin).output().expect("run generated binary");
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}

/// Imports only some programs have: reading stdin and the clocks.
const STDIO_IMPORTS: &str = r#"
  (import "wasi:io/streams@0.2.0" "[method]input-stream.read" (func $read (param i32 i64 i32)))
  (import "wasi:io/streams@0.2.0" "[method]input-stream.subscribe" (func $sub_in (param i32) (result i32)))
  (import "wasi:clocks/monotonic-clock@0.2.0" "now" (func $now (result i64)))
  (import "wasi:clocks/monotonic-clock@0.2.0" "subscribe-instant" (func $sub_at (param i64) (result i32)))
  (import "wasi:clocks/wall-clock@0.2.0" "now" (func $wall (param i32)))
"#;

/// `read_stdin` reads `result<list<u8>, stream-error>` (tag at 200; the list at 204,
/// or for `closed` the error tag at 204) from a stdin that holds "ab\ncd".
/// `use_clock` sleeps through `poll` and `block` on deadline pollables, and checks
/// that `poll` returns at once when one pollable is ready and another is far off.
const STDIO: &str = r#"
  (func (export "read_stdin")
    ;; a zero length is an empty list, not the end of input
    (call $read (i32.const 0) (i64.const 0) (i32.const 200))
    (if (i32.load8_u (i32.const 200)) (then unreachable))
    (if (i32.load (i32.const 208)) (then unreachable))
    (call $read (i32.const 0) (i64.const 4) (i32.const 200))
    (if (i32.load8_u (i32.const 200)) (then unreachable))
    (if (i32.ne (i32.load (i32.const 208)) (i32.const 4)) (then unreachable))
    (if (i32.ne (i32.load (i32.load (i32.const 204))) (i32.const 0x630a6261)) (then unreachable))
    ;; a length of u64::MAX (-1 as an i64) means "as much as there is"
    (call $read (i32.const 0) (i64.const -1) (i32.const 200))
    (if (i32.load8_u (i32.const 200)) (then unreachable))
    (if (i32.ne (i32.load (i32.const 208)) (i32.const 1)) (then unreachable))
    (if (i32.ne (i32.load8_u (i32.load (i32.const 204))) (i32.const 100)) (then unreachable))
    (call $read (i32.const 0) (i64.const 4) (i32.const 200))
    (if (i32.ne (i32.load8_u (i32.const 200)) (i32.const 1)) (then unreachable))
    (if (i32.ne (i32.load8_u (i32.const 204)) (i32.const 1)) (then unreachable)))
  (func (export "use_clock") (local $t0 i64) (local $t1 i64)
    (local.set $t0 (call $now))
    (i32.store (i32.const 96) (call $sub_dur (i64.const 50000000)))
    (i64.store (i32.const 104) (i64.const -1))
    (call $poll (i32.const 96) (i32.const 1) (i32.const 104))
    (if (i32.ne (i32.load (i32.const 108)) (i32.const 1)) (then unreachable))
    (local.set $t1 (call $now))
    (if (i64.lt_u (i64.sub (local.get $t1) (local.get $t0)) (i64.const 50000000)) (then unreachable))
    ;; an instant 30 ms ahead: block waits for it
    (call $block (call $sub_at (i64.add (local.get $t1) (i64.const 30000000))))
    (if (i64.lt_u (i64.sub (call $now) (local.get $t1)) (i64.const 30000000)) (then unreachable))
    ;; an instant already past is ready at once (read as a duration it would wait t1)
    (local.set $t0 (call $now))
    (call $block (call $sub_at (local.get $t1)))
    (if (i64.gt_u (i64.sub (call $now) (local.get $t0)) (i64.const 25000000)) (then unreachable))
    ;; a ten-second deadline and a ready stream: poll returns [1] without waiting
    (i32.store (i32.const 96) (call $sub_dur (i64.const 10000000000)))
    (i32.store (i32.const 100) (call $sub_in (i32.const 0)))
    (call $poll (i32.const 96) (i32.const 2) (i32.const 104))
    (if (i32.ne (i32.load (i32.const 108)) (i32.const 1)) (then unreachable))
    (if (i32.ne (i32.load (i32.load (i32.const 104))) (i32.const 1)) (then unreachable))
    ;; wall-clock now: seconds (u64) at 120, nanoseconds (u32) at 128
    (i32.store (i32.const 128) (i32.const -1))
    (call $wall (i32.const 120))
    (if (i64.lt_u (i64.load (i32.const 120)) (i64.const 1700000000)) (then unreachable))
    (if (i64.gt_u (i64.load (i32.const 120)) (i64.const 100000000000)) (then unreachable))
    (if (i32.ge_u (i32.load (i32.const 128)) (i32.const 1000000000)) (then unreachable)))
"#;

/// `HELLO` with extra imports (before its memory) and functions (before `bye`).
fn hello_with(imports: &str, funcs: &str) -> String {
    HELLO
        .replacen(
            "(memory (export",
            &format!("{imports}\n  (memory (export"),
            1,
        )
        .replacen(
            "(func (export \"bye\")",
            &format!("{funcs}\n  (func (export \"bye\")"),
            1,
        )
}

fn stdio_wat() -> String {
    hello_with(STDIO_IMPORTS, STDIO)
}

#[test]
fn a_host_reads_stdin_and_reports_end_of_input_as_closed() {
    use std::io::Write;
    use std::process::Stdio;
    let bin = compile(
        "stdin",
        &stdio_wat(),
        "P2Host::new(vec![], vec![])",
        "inst.read_stdin();",
    );
    let mut child = Command::new(&bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn generated binary");
    child.stdin.take().unwrap().write_all(b"ab\ncd").unwrap();
    let run = child.wait_with_output().expect("run generated binary");
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn polling_deadline_pollables_waits_for_the_clock() {
    let bin = compile(
        "clocks",
        &stdio_wat(),
        "P2Host::new(vec![], vec![])",
        "inst.use_clock();",
    );
    let started = std::time::Instant::now();
    let run = Command::new(&bin).output().expect("run generated binary");
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    // 50 ms + 30 ms of waiting, and not the ten-second deadline.
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
}

/// The `wasi:random` imports: `insecure-seed` is what a `HashMap` needs; the others are
/// what `rand`/`getrandom`-style programs call.
const RANDOM_IMPORTS: &str = r#"
  (import "wasi:random/random@0.2.0" "get-random-bytes" (func $bytes (param i64 i32)))
  (import "wasi:random/random@0.2.0" "get-random-u64" (func $u64 (result i64)))
  (import "wasi:random/insecure@0.2.0" "get-insecure-random-bytes" (func $ibytes (param i64 i32)))
  (import "wasi:random/insecure@0.2.0" "get-insecure-random-u64" (func $iu64 (result i64)))
  (import "wasi:random/insecure-seed@0.2.0" "insecure-seed" (func $seed (param i32)))
"#;

/// `random` checks `list<u8>` results (ptr, len at the return pointer) and the
/// `tuple<u64, u64>` seed (at the return pointer and 8 bytes after it).
const RANDOM: &str = r#"
  (func (export "random") (local $a i64) (local $b i64)
    (call $bytes (i64.const 16) (i32.const 200))
    (if (i32.ne (i32.load (i32.const 204)) (i32.const 16)) (then unreachable))
    (local.set $a (i64.load (i32.load (i32.const 200))))
    (local.set $b (i64.load offset=8 (i32.load (i32.const 200))))
    (call $bytes (i64.const 16) (i32.const 200))
    (if (i64.eq (local.get $a) (i64.load (i32.load (i32.const 200)))) (then unreachable))
    (if (i64.eq (local.get $b) (i64.load offset=8 (i32.load (i32.const 200)))) (then unreachable))
    ;; no bytes: an empty list
    (call $bytes (i64.const 0) (i32.const 200))
    (if (i32.load (i32.const 204)) (then unreachable))
    (call $ibytes (i64.const 5) (i32.const 200))
    (if (i32.ne (i32.load (i32.const 204)) (i32.const 5)) (then unreachable))
    (if (i32.lt_u (i32.load (i32.const 200)) (i32.const 1024)) (then unreachable))
    (if (i64.eq (call $u64) (call $u64)) (then unreachable))
    (if (i64.eq (call $iu64) (call $iu64)) (then unreachable))
    ;; the seed: two u64s, both written
    (i64.store (i32.const 220) (i64.const 0))
    (i64.store (i32.const 228) (i64.const 0))
    (call $seed (i32.const 220))
    (if (i64.eqz (i64.load (i32.const 220))) (then unreachable))
    (if (i64.eqz (i64.load (i32.const 228))) (then unreachable)))
"#;

#[test]
fn a_host_serves_random_bytes_numbers_and_the_insecure_seed() {
    let wat = hello_with(RANDOM_IMPORTS, RANDOM);
    let bin = compile(
        "random",
        &wat,
        "P2Host::new(vec![], vec![])",
        "inst.random();",
    );
    let run = Command::new(&bin).output().expect("run generated binary");
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}

/// The imports and path strings (at 300 to 400) a program needs to open a file and
/// read it.
const FS_IMPORTS: &str = r#"
  (import "wasi:filesystem/types@0.2.0" "[resource-drop]descriptor" (func $drop_fd (param i32)))
  (import "wasi:filesystem/preopens@0.2.0" "get-directories" (func $dirs (param i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.open-at" (func $open (param i32 i32 i32 i32 i32 i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.read-via-stream" (func $rvs (param i32 i64 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.write-via-stream" (func $wvs (param i32 i64 i32)))
  (import "wasi:io/streams@0.2.0" "[method]input-stream.read" (func $read (param i32 i64 i32)))
  (data (i32.const 300) "hello.txt")
  (data (i32.const 320) "missing")
  (data (i32.const 340) "../wasm2rs_wasip2_fs_secret.txt")
  (data (i32.const 380) "/etc/passwd")
  (data (i32.const 400) "link.txt")
  (data (i32.const 420) "../wasm2rs_wasip2_fs_root/hello.txt")
"#;

/// `fs_read` walks the file API on a root holding `hello.txt` ("hello fs") and a
/// symlink `link.txt` to a file outside it:
/// `get-directories` is `list<tuple<descriptor, string>>` (12-byte elements),
/// `open-at` and `read-via-stream` are `result<handle, error-code>` (the handle or the
/// error's tag byte at `ret + 4`). Error codes: no-entry 20, not-permitted 31, read-only 33.
const FS: &str = r#"
  (func (export "fs_read") (local $dir i32) (local $f i32) (local $s i32)
    (call $dirs (i32.const 200))
    (if (i32.ne (i32.load (i32.const 204)) (i32.const 1)) (then unreachable))
    (local.set $dir (i32.load (i32.load (i32.const 200))))
    ;; the preopen's name is "/"
    (if (i32.ne (i32.load offset=8 (i32.load (i32.const 200))) (i32.const 1)) (then unreachable))
    (if (i32.ne (i32.load8_u (i32.load offset=4 (i32.load (i32.const 200)))) (i32.const 47)) (then unreachable))
    (call $open (local.get $dir) (i32.const 1) (i32.const 300) (i32.const 9) (i32.const 0) (i32.const 1) (i32.const 220))
    (if (i32.load8_u (i32.const 220)) (then unreachable))
    (local.set $f (i32.load (i32.const 224)))
    (call $rvs (local.get $f) (i64.const 0) (i32.const 240))
    (if (i32.load8_u (i32.const 240)) (then unreachable))
    (local.set $s (i32.load (i32.const 244)))
    ;; "hell", then the rest, then end of file
    (call $read (local.get $s) (i64.const 4) (i32.const 260))
    (if (i32.load8_u (i32.const 260)) (then unreachable))
    (if (i32.ne (i32.load (i32.load (i32.const 264))) (i32.const 0x6c6c6568)) (then unreachable))
    (call $read (local.get $s) (i64.const -1) (i32.const 260))
    (if (i32.ne (i32.load (i32.const 268)) (i32.const 4)) (then unreachable))
    (call $read (local.get $s) (i64.const 1) (i32.const 260))
    (if (i32.ne (i32.load8_u (i32.const 260)) (i32.const 1)) (then unreachable))
    (if (i32.ne (i32.load8_u (i32.const 264)) (i32.const 1)) (then unreachable))
    ;; a second stream at offset 6 starts at "fs"
    (call $rvs (local.get $f) (i64.const 6) (i32.const 240))
    (call $read (i32.load (i32.const 244)) (i64.const 8) (i32.const 260))
    (if (i32.ne (i32.load (i32.const 268)) (i32.const 2)) (then unreachable))
    (if (i32.ne (i32.load16_u (i32.load (i32.const 264))) (i32.const 0x7366)) (then unreachable))
    ;; no such file; a path out of the root; an absolute path
    (call $open (local.get $dir) (i32.const 1) (i32.const 320) (i32.const 7) (i32.const 0) (i32.const 1) (i32.const 220))
    (if (i32.ne (i32.load8_u (i32.const 220)) (i32.const 1)) (then unreachable))
    (if (i32.ne (i32.load8_u (i32.const 224)) (i32.const 20)) (then unreachable))
    (call $open (local.get $dir) (i32.const 1) (i32.const 340) (i32.const 31) (i32.const 0) (i32.const 1) (i32.const 220))
    (if (i32.ne (i32.load8_u (i32.const 224)) (i32.const 31)) (then unreachable))
    (call $open (local.get $dir) (i32.const 1) (i32.const 380) (i32.const 11) (i32.const 0) (i32.const 1) (i32.const 220))
    (if (i32.ne (i32.load8_u (i32.const 224)) (i32.const 31)) (then unreachable))
    ;; `..` is refused even where it would lead back into the root
    (call $open (local.get $dir) (i32.const 1) (i32.const 420) (i32.const 35) (i32.const 0) (i32.const 1) (i32.const 220))
    (if (i32.ne (i32.load8_u (i32.const 224)) (i32.const 31)) (then unreachable))
    ;; a symlink under the root that leads out of it
    (call $open (local.get $dir) (i32.const 1) (i32.const 400) (i32.const 8) (i32.const 0) (i32.const 1) (i32.const 220))
    (if (i32.ne (i32.load8_u (i32.const 224)) (i32.const 31)) (then unreachable))
    ;; the host is read-only
    (call $wvs (local.get $f) (i64.const 0) (i32.const 240))
    (if (i32.ne (i32.load8_u (i32.const 240)) (i32.const 1)) (then unreachable))
    (if (i32.ne (i32.load8_u (i32.const 244)) (i32.const 33)) (then unreachable))
    (call $drop_fd (local.get $f)))
"#;

#[cfg(unix)]
#[test]
fn a_host_opens_and_reads_files_under_its_preopened_directory() {
    let root = std::env::temp_dir().join("wasm2rs_wasip2_fs_root");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("hello.txt"), "hello fs").unwrap();
    // a file next to the root, which `../` would reach
    let secret = root.parent().unwrap().join("wasm2rs_wasip2_fs_secret.txt");
    std::fs::write(&secret, "no").unwrap();
    let link = root.join("link.txt");
    std::fs::remove_file(&link).ok();
    std::os::unix::fs::symlink(&secret, &link).unwrap();
    let init = format!(
        "P2Host::new(vec![], vec![]).with_preopen(std::path::PathBuf::from({:?}))",
        root.to_str().unwrap()
    );
    let bin = compile(
        "fs_read",
        &hello_with(FS_IMPORTS, FS),
        &init,
        "inst.fs_read();",
    );
    let run = Command::new(&bin).output().expect("run generated binary");
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}
