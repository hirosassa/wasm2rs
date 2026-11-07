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
  (import "wasi:io/poll@0.2.0" "[resource-drop]pollable" (func $drop_pollable (param i32)))
  (import "wasi:io/streams@0.2.0" "[resource-drop]input-stream" (func $drop_stream (param i32)))
  (import "wasi:io/streams@0.2.0" "[resource-drop]output-stream" (func $drop_out (param i32)))
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
    run_ok(&bin);
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
  ;; waits until stdin has data or is at its end: a fresh pollable, as `std` does
  (func $wait_stdin
    (i32.store (i32.const 112) (call $sub_in (i32.const 0)))
    (call $poll (i32.const 112) (i32.const 1) (i32.const 116)))
  ;; polls stdin and a pollable `ns` nanoseconds away: returns the one index that is ready
  (func $poll_stdin_or_after (param $ns i64) (result i32)
    (i32.store (i32.const 96) (call $sub_in (i32.const 0)))
    (i32.store (i32.const 100) (call $sub_dur (local.get $ns)))
    (call $poll (i32.const 96) (i32.const 2) (i32.const 104))
    (if (i32.ne (i32.load (i32.const 108)) (i32.const 1)) (then unreachable))
    (i32.load (i32.load (i32.const 104))))
  ;; nothing has been written to stdin (and it is still open): `read` returns an empty
  ;; list at once, and `poll` on it times out with the 30 ms deadline
  (func (export "idle_stdin")
    (call $read (i32.const 0) (i64.const 4) (i32.const 200))
    (if (i32.load8_u (i32.const 200)) (then unreachable))
    (if (i32.load (i32.const 208)) (then unreachable))
    (if (i32.ne (call $poll_stdin_or_after (i64.const 30000000)) (i32.const 1)) (then unreachable)))
  ;; nothing arrives for 200 ms: a 100 ms deadline wins (`[1]`). Then "x" arrives while
  ;; `poll` waits (the other pollable is ten seconds away), and the input ends:
  ;; `poll` returns for the end too, and `read` reports `closed`
  (func (export "wait_stdin_data")
    ;; tell the test (with "hi\n") that the program is running
    (call $write (call $stdout) (i32.const 64) (i32.const 3) (i32.const 32))
    (if (i32.ne (call $poll_stdin_or_after (i64.const 100000000)) (i32.const 1)) (then unreachable))
    (if (call $poll_stdin_or_after (i64.const 10000000000)) (then unreachable))
    (call $read (i32.const 0) (i64.const 16) (i32.const 200))
    (if (i32.load8_u (i32.const 200)) (then unreachable))
    (if (i32.ne (i32.load (i32.const 208)) (i32.const 1)) (then unreachable))
    (if (i32.ne (i32.load8_u (i32.load (i32.const 204))) (i32.const 120)) (then unreachable))
    (call $wait_stdin)
    (call $read (i32.const 0) (i64.const 16) (i32.const 200))
    (if (i32.ne (i32.load8_u (i32.const 200)) (i32.const 1)) (then unreachable))
    (if (i32.ne (i32.load8_u (i32.const 204)) (i32.const 1)) (then unreachable)))
  (func (export "read_stdin")
    (call $wait_stdin)
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
    (call $wait_stdin)
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

/// Fails with `run`'s stderr unless it exited successfully.
fn assert_succeeded(run: &std::process::Output) {
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}

/// Runs `bin` and fails with its stderr unless it exits successfully.
fn run_ok(bin: &std::path::Path) {
    assert_succeeded(&Command::new(bin).output().expect("run generated binary"));
}

/// The host expression for a program that sees `root` as its read-only `/`.
fn preopen_host(root: &std::path::Path) -> String {
    preopen_host_with(root, "with_preopen")
}

/// Like `preopen_host`, through the constructor `method` (`with_writable_preopen`
/// lets the program write).
fn preopen_host_with(root: &std::path::Path, method: &str) -> String {
    format!(
        "P2Host::new(vec![], vec![]).{method}(std::path::PathBuf::from({:?}))",
        root.to_str().unwrap()
    )
}

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

/// Starts `bin` with all three standard streams piped.
fn spawn_piped(bin: &std::path::Path) -> std::process::Child {
    use std::process::Stdio;
    Command::new(bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn generated binary")
}

/// Waits up to 30 seconds (the machine may be busy) for `child` to exit successfully;
/// kills it and fails if it does not exit, or fails.
fn finish_within_30_seconds(mut child: std::process::Child) {
    let started = std::time::Instant::now();
    while child.try_wait().unwrap().is_none() {
        if started.elapsed() > std::time::Duration::from_secs(30) {
            child.kill().unwrap();
            panic!("the generated binary blocked");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_succeeded(&child.wait_with_output().unwrap());
}

#[test]
fn a_host_reads_stdin_and_reports_end_of_input_as_closed() {
    use std::io::Write;
    let bin = compile(
        "stdin",
        &stdio_wat(),
        "P2Host::new(vec![], vec![])",
        "inst.read_stdin();",
    );
    let mut child = spawn_piped(&bin);
    child.stdin.take().unwrap().write_all(b"ab\ncd").unwrap();
    finish_within_30_seconds(child);
}

#[test]
fn reading_an_idle_stdin_returns_an_empty_list_without_blocking() {
    let bin = compile(
        "stdin_idle",
        &stdio_wat(),
        "P2Host::new(vec![], vec![])",
        "inst.idle_stdin();",
    );
    let mut child = spawn_piped(&bin);
    // the write end stays open, so stdin is idle, not at its end
    let _stdin = child.stdin.take().unwrap();
    finish_within_30_seconds(child);
}

#[test]
fn polling_stdin_wakes_when_data_arrives_and_at_the_end_of_input() {
    use std::io::{Read, Write};
    let bin = compile(
        "stdin_wake",
        &stdio_wat(),
        "P2Host::new(vec![], vec![])",
        "inst.wait_stdin_data();",
    );
    let mut child = spawn_piped(&bin);
    let mut stdin = child.stdin.take().unwrap();
    // the program is up (its first poll, with a 100 ms deadline, follows at once)
    let mut hi = [0u8; 3];
    child.stdout.as_mut().unwrap().read_exact(&mut hi).unwrap();
    // long enough for the 100 ms deadline to win even on a loaded machine
    std::thread::sleep(std::time::Duration::from_millis(500));
    stdin.write_all(b"x").unwrap();
    // let the program read "x" before the input ends
    std::thread::sleep(std::time::Duration::from_millis(200));
    drop(stdin);
    // a `poll` that missed the data would wait out its ten-second deadline and the guest's
    // checks would fail
    finish_within_30_seconds(child);
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
    run_ok(&bin);
    // 50 ms + 30 ms of waiting, and not the ten-second deadline.
    assert!(started.elapsed() < std::time::Duration::from_secs(9));
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
    run_ok(&bin);
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
    let init = preopen_host(&root);
    let bin = compile(
        "fs_read",
        &hello_with(FS_IMPORTS, FS),
        &init,
        "inst.fs_read();",
    );
    run_ok(&bin);
}

/// The imports and path strings (at 300 to 340) the metadata and directory tests use.
const STAT_IMPORTS: &str = r#"
  (import "wasi:filesystem/preopens@0.2.0" "get-directories" (func $dirs (param i32)))
  (import "wasi:filesystem/types@0.2.0" "[resource-drop]descriptor" (func (param i32)))
  (import "wasi:filesystem/types@0.2.0" "[resource-drop]directory-entry-stream" (func (param i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.open-at" (func $open (param i32 i32 i32 i32 i32 i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.stat" (func $stat (param i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.stat-at" (func $stat_at (param i32 i32 i32 i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.get-flags" (func $flags (param i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.metadata-hash" (func $hash (param i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.metadata-hash-at" (func $hash_at (param i32 i32 i32 i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.read-directory" (func $readdir (param i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]directory-entry-stream.read-directory-entry" (func $entry (param i32 i32)))
  (data (i32.const 300) "a.txt")
  (data (i32.const 310) "sub")
  (data (i32.const 320) "missing")
  (data (i32.const 330) "../x")
  (data (i32.const 340) "link.txt")
"#;

/// `fs_stat` runs on a root holding `a.txt` ("hello") and `sub/` (with `b.txt` and
/// `c.txt`). `descriptor-stat` sits at `ret + 8` of its result: type (6 = regular
/// file, 3 = directory), link count, size, then three `option<datetime>` of 24 bytes
/// from `ret + 32` (tag, seconds, nanoseconds). `metadata-hash` is two u64s at
/// `ret + 8`. `read-directory-entry` is `result<option<entry>>`: the option's tag at
/// `ret + 4`, the entry (type byte, name `(ptr, len)`) at `ret + 8`.
const STAT: &str = r#"
  (func (export "fs_stat") (local $dir i32) (local $sub i32) (local $file i32) (local $stream i32)
        (local $count i32) (local $sum i32) (local $h i64)
    (call $dirs (i32.const 200))
    (local.set $dir (i32.load (i32.load (i32.const 200))))
    ;; a file by path: regular file, size 5, one link, a plausible modification time
    (call $stat_at (local.get $dir) (i32.const 1) (i32.const 300) (i32.const 5) (i32.const 600))
    (if (i32.load8_u (i32.const 600)) (then unreachable))
    (if (i32.ne (i32.load8_u (i32.const 608)) (i32.const 6)) (then unreachable))
    (if (i64.eqz (i64.load (i32.const 616))) (then unreachable))
    (if (i64.ne (i64.load (i32.const 624)) (i64.const 5)) (then unreachable))
    (if (i32.ne (i32.load8_u (i32.const 632)) (i32.const 1)) (then unreachable))
    (if (i32.ne (i32.load8_u (i32.const 656)) (i32.const 1)) (then unreachable))
    (if (i32.load8_u (i32.const 680)) (then unreachable))
    (if (i64.lt_u (i64.load (i32.const 664)) (i64.const 1700000000)) (then unreachable))
    (if (i32.ge_u (i32.load (i32.const 672)) (i32.const 1000000000)) (then unreachable))
    ;; a directory by path, and the root descriptor itself
    (call $stat_at (local.get $dir) (i32.const 1) (i32.const 310) (i32.const 3) (i32.const 600))
    (if (i32.ne (i32.load8_u (i32.const 608)) (i32.const 3)) (then unreachable))
    (call $stat (local.get $dir) (i32.const 600))
    (if (i32.load8_u (i32.const 600)) (then unreachable))
    (if (i32.ne (i32.load8_u (i32.const 608)) (i32.const 3)) (then unreachable))
    ;; no such file; a path out of the root
    (call $stat_at (local.get $dir) (i32.const 1) (i32.const 320) (i32.const 7) (i32.const 600))
    (if (i32.ne (i32.load8_u (i32.const 600)) (i32.const 1)) (then unreachable))
    (if (i32.ne (i32.load8_u (i32.const 608)) (i32.const 20)) (then unreachable))
    (call $stat_at (local.get $dir) (i32.const 1) (i32.const 330) (i32.const 4) (i32.const 600))
    (if (i32.ne (i32.load8_u (i32.const 608)) (i32.const 31)) (then unreachable))
    ;; a symlink to a file outside the root
    (call $stat_at (local.get $dir) (i32.const 1) (i32.const 340) (i32.const 8) (i32.const 600))
    (if (i32.ne (i32.load8_u (i32.const 608)) (i32.const 31)) (then unreachable))
    ;; descriptor-flags: `read` (1), the flags byte at ret + 1
    (call $flags (local.get $dir) (i32.const 700))
    (if (i32.load8_u (i32.const 700)) (then unreachable))
    (if (i32.ne (i32.load8_u (i32.const 701)) (i32.const 1)) (then unreachable))
    ;; metadata-hash: the same file twice gives the same value, another file another
    (call $hash_at (local.get $dir) (i32.const 1) (i32.const 300) (i32.const 5) (i32.const 720))
    (if (i32.load8_u (i32.const 720)) (then unreachable))
    (local.set $h (i64.load (i32.const 728)))
    (call $hash_at (local.get $dir) (i32.const 1) (i32.const 300) (i32.const 5) (i32.const 760))
    (if (i64.ne (local.get $h) (i64.load (i32.const 768))) (then unreachable))
    (if (i64.ne (i64.load (i32.const 736)) (i64.load (i32.const 776))) (then unreachable))
    (call $hash_at (local.get $dir) (i32.const 1) (i32.const 310) (i32.const 3) (i32.const 760))
    (if (i64.eq (local.get $h) (i64.load (i32.const 768))) (then unreachable))
    (call $hash (local.get $dir) (i32.const 760))
    (if (i32.load8_u (i32.const 760)) (then unreachable))
    ;; read-directory of `sub`: two regular files, b.txt and c.txt, then none
    (call $open (local.get $dir) (i32.const 1) (i32.const 310) (i32.const 3) (i32.const 2) (i32.const 1) (i32.const 220))
    (if (i32.load8_u (i32.const 220)) (then unreachable))
    (local.set $sub (i32.load (i32.const 224)))
    (call $readdir (local.get $sub) (i32.const 800))
    (if (i32.load8_u (i32.const 800)) (then unreachable))
    (local.set $stream (i32.load (i32.const 804)))
    (loop $next
      (call $entry (local.get $stream) (i32.const 820))
      (if (i32.load8_u (i32.const 820)) (then unreachable))
      (if (i32.load8_u (i32.const 824))
        (then
          (if (i32.ne (i32.load8_u (i32.const 828)) (i32.const 6)) (then unreachable))
          (if (i32.ne (i32.load (i32.const 836)) (i32.const 5)) (then unreachable))
          (local.set $sum (i32.add (local.get $sum) (i32.load8_u (i32.load (i32.const 832)))))
          (local.set $count (i32.add (local.get $count) (i32.const 1)))
          (br $next))))
    (if (i32.ne (local.get $count) (i32.const 2)) (then unreachable))
    (if (i32.ne (local.get $sum) (i32.const 197)) (then unreachable))
    ;; open-at with the directory flag on a file, and read-directory of a file, are
    ;; not-directory (24)
    (call $open (local.get $dir) (i32.const 1) (i32.const 300) (i32.const 5) (i32.const 2) (i32.const 1) (i32.const 220))
    (if (i32.ne (i32.load8_u (i32.const 224)) (i32.const 24)) (then unreachable))
    (call $open (local.get $dir) (i32.const 1) (i32.const 300) (i32.const 5) (i32.const 0) (i32.const 1) (i32.const 220))
    (if (i32.load8_u (i32.const 220)) (then unreachable))
    (local.set $file (i32.load (i32.const 224)))
    (call $readdir (local.get $file) (i32.const 800))
    (if (i32.ne (i32.load8_u (i32.const 800)) (i32.const 1)) (then unreachable))
    (if (i32.ne (i32.load8_u (i32.const 804)) (i32.const 24)) (then unreachable)))
"#;

#[cfg(unix)]
#[test]
fn a_host_stats_files_and_lists_directories_under_its_preopen() {
    let root = std::env::temp_dir().join("wasm2rs_wasip2_fs_stat_root");
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::write(root.join("a.txt"), "hello").unwrap();
    std::fs::write(root.join("sub/b.txt"), "b").unwrap();
    std::fs::write(root.join("sub/c.txt"), "c").unwrap();
    // a file next to the root, which `../` would reach
    let secret = root
        .parent()
        .unwrap()
        .join("wasm2rs_wasip2_fs_stat_secret.txt");
    std::fs::write(&secret, "no").unwrap();
    let link = root.join("link.txt");
    std::fs::remove_file(&link).ok();
    std::os::unix::fs::symlink(&secret, &link).unwrap();
    let init = preopen_host(&root);
    let bin = compile(
        "fs_stat",
        &hello_with(STAT_IMPORTS, STAT),
        &init,
        "inst.fs_stat();",
    );
    run_ok(&bin);
}

/// What `fs_reuse` needs beyond `FS_IMPORTS`.
const REUSE_IMPORTS: &str = r#"
  (import "wasi:filesystem/types@0.2.0" "[resource-drop]directory-entry-stream" (func $drop_ls (param i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.read-directory" (func $readdir (param i32 i32)))
"#;

/// `fs_reuse` drops each kind of handle and takes a new one: the host reuses a
/// dropped slot (so its tables do not grow), never hands out a handle that is still
/// open, and keeps the preopen even if the guest drops it. Results are
/// `result<handle, error-code>` (the handle at `ret + 4`).
const REUSE: &str = r#"
  (func (export "fs_reuse") (local $dir i32) (local $f1 i32) (local $s i32) (local $l i32) (local $p i32)
    (call $dirs (i32.const 200))
    (local.set $dir (i32.load (i32.load (i32.const 200))))
    ;; a descriptor: the slot comes back once dropped, but not while it is open
    (call $open (local.get $dir) (i32.const 1) (i32.const 300) (i32.const 9) (i32.const 0) (i32.const 1) (i32.const 220))
    (local.set $f1 (i32.load (i32.const 224)))
    (call $open (local.get $dir) (i32.const 1) (i32.const 300) (i32.const 9) (i32.const 0) (i32.const 1) (i32.const 220))
    (if (i32.eq (i32.load (i32.const 224)) (local.get $f1)) (then unreachable))
    (call $drop_fd (local.get $f1))
    (call $open (local.get $dir) (i32.const 1) (i32.const 300) (i32.const 9) (i32.const 0) (i32.const 1) (i32.const 220))
    (if (i32.ne (i32.load (i32.const 224)) (local.get $f1)) (then unreachable))
    ;; an input stream
    (call $rvs (local.get $f1) (i64.const 0) (i32.const 240))
    (local.set $s (i32.load (i32.const 244)))
    (call $drop_stream (local.get $s))
    (call $rvs (local.get $f1) (i64.const 0) (i32.const 240))
    (if (i32.ne (i32.load (i32.const 244)) (local.get $s)) (then unreachable))
    (call $read (local.get $s) (i64.const 4) (i32.const 260))
    (if (i32.ne (i32.load (i32.load (i32.const 264))) (i32.const 0x6c6c6568)) (then unreachable))
    ;; a directory listing
    (call $readdir (local.get $dir) (i32.const 800))
    (local.set $l (i32.load (i32.const 804)))
    (call $drop_ls (local.get $l))
    (call $readdir (local.get $dir) (i32.const 800))
    (if (i32.ne (i32.load (i32.const 804)) (local.get $l)) (then unreachable))
    ;; a pollable
    (local.set $p (call $sub_dur (i64.const 0)))
    (call $drop_pollable (local.get $p))
    (if (i32.ne (call $sub_dur (i64.const 0)) (local.get $p)) (then unreachable))
    ;; the preopen is never freed
    (call $drop_fd (local.get $dir))
    (call $open (local.get $dir) (i32.const 1) (i32.const 300) (i32.const 9) (i32.const 0) (i32.const 1) (i32.const 220))
    (if (i32.load8_u (i32.const 220)) (then unreachable))
    (if (i32.eq (i32.load (i32.const 224)) (local.get $dir)) (then unreachable)))
"#;

#[test]
fn a_host_reuses_the_slots_of_dropped_handles() {
    let root = std::env::temp_dir().join("wasm2rs_wasip2_reuse_root");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("hello.txt"), "hello fs").unwrap();
    let init = preopen_host(&root);
    let wat = hello_with(&format!("{FS_IMPORTS}{REUSE_IMPORTS}"), REUSE);
    let bin = compile("fs_reuse", &wat, &init, "inst.fs_reuse();");
    run_ok(&bin);
}

/// The imports and data `fs_write` needs: paths at 300 to 396 and 450 to 466, contents at 400 to 440.
const WRITE_IMPORTS: &str = r#"
  (import "wasi:filesystem/preopens@0.2.0" "get-directories" (func $dirs (param i32)))
  (import "wasi:filesystem/types@0.2.0" "[resource-drop]descriptor" (func $drop_fd (param i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.open-at" (func $open (param i32 i32 i32 i32 i32 i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.write-via-stream" (func $wvs (param i32 i64 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.append-via-stream" (func $avs (param i32 i32)))
  (data (i32.const 300) "new.txt")
  (data (i32.const 310) "../wasm2rs_w_escape.txt")
  (data (i32.const 340) "/abs.txt")
  (data (i32.const 350) "dangle")
  (data (i32.const 360) "sub/in.txt")
  (data (i32.const 372) "trunc.txt")
  (data (i32.const 384) "outdir/x.txt")
  (data (i32.const 450) "newd/")
  (data (i32.const 460) "newd/.")
  (data (i32.const 400) "hello")
  (data (i32.const 410) "EY")
  (data (i32.const 420) " world")
  (data (i32.const 430) "in")
  (data (i32.const 440) "N")
"#;

/// `fs_write` runs on a writable root holding `trunc.txt` ("old contents"), `sub/`, a
/// symlink `dangle` to a file outside the root that does not exist and a symlink
/// `outdir` to a directory outside it. open-flags:
/// create 1, exclusive 4, truncate 8; descriptor-flags: read 1, write 2 (so `3` is
/// read + write); path-flags 1 follows links. `put_via` writes `(ptr, len)` to a
/// stream from `write-via-stream` at `offset` (or `append-via-stream` for `-1`), then
/// flushes and drops the stream. Error codes: exist 7, invalid 12, not-permitted 31.
const WRITE: &str = r#"
  (func $open_at (param $dir i32) (param $path i32) (param $len i32) (param $oflags i32) (param $dflags i32)
    (call $open (local.get $dir) (i32.const 1) (local.get $path) (local.get $len)
                (local.get $oflags) (local.get $dflags) (i32.const 220)))
  (func $expect_err (param $code i32)
    (if (i32.ne (i32.load8_u (i32.const 220)) (i32.const 1)) (then unreachable))
    (if (i32.ne (i32.load8_u (i32.const 224)) (local.get $code)) (then unreachable)))
  (func $put_via (param $f i32) (param $offset i64) (param $ptr i32) (param $len i32) (local $s i32)
    (if (i64.lt_s (local.get $offset) (i64.const 0))
      (then (call $avs (local.get $f) (i32.const 240)))
      (else (call $wvs (local.get $f) (local.get $offset) (i32.const 240))))
    (if (i32.load8_u (i32.const 240)) (then unreachable))
    (local.set $s (i32.load (i32.const 244)))
    (call $check (local.get $s) (i32.const 260))
    (if (i32.load8_u (i32.const 260)) (then unreachable))
    (if (i64.lt_u (i64.load (i32.const 268)) (i64.extend_i32_u (local.get $len))) (then unreachable))
    (call $write (local.get $s) (local.get $ptr) (local.get $len) (i32.const 280))
    (if (i32.load8_u (i32.const 280)) (then unreachable))
    (call $flush (local.get $s) (i32.const 280))
    (if (i32.load8_u (i32.const 280)) (then unreachable))
    (call $drop_out (local.get $s)))
  (func (export "fs_write") (local $dir i32) (local $f i32)
    (call $dirs (i32.const 200))
    (local.set $dir (i32.load (i32.load (i32.const 200))))
    ;; create a file and write it from offset 0
    (call $open_at (local.get $dir) (i32.const 300) (i32.const 7) (i32.const 1) (i32.const 3))
    (if (i32.load8_u (i32.const 220)) (then unreachable))
    (local.set $f (i32.load (i32.const 224)))
    (call $put_via (local.get $f) (i64.const 0) (i32.const 400) (i32.const 5))
    (call $drop_fd (local.get $f))
    ;; overwrite from offset 1, then append
    (call $open_at (local.get $dir) (i32.const 300) (i32.const 7) (i32.const 0) (i32.const 2))
    (local.set $f (i32.load (i32.const 224)))
    (call $put_via (local.get $f) (i64.const 1) (i32.const 410) (i32.const 2))
    (call $put_via (local.get $f) (i64.const -1) (i32.const 420) (i32.const 6))
    (call $drop_fd (local.get $f))
    ;; a file in a subdirectory
    (call $open_at (local.get $dir) (i32.const 360) (i32.const 10) (i32.const 1) (i32.const 3))
    (if (i32.load8_u (i32.const 220)) (then unreachable))
    (local.set $f (i32.load (i32.const 224)))
    (call $put_via (local.get $f) (i64.const 0) (i32.const 430) (i32.const 2))
    (call $drop_fd (local.get $f))
    ;; truncate an existing file, then write
    (call $open_at (local.get $dir) (i32.const 372) (i32.const 9) (i32.const 8) (i32.const 2))
    (if (i32.load8_u (i32.const 220)) (then unreachable))
    (local.set $f (i32.load (i32.const 224)))
    (call $put_via (local.get $f) (i64.const 0) (i32.const 440) (i32.const 1))
    (call $drop_fd (local.get $f))
    ;; create + exclusive on an existing file
    (call $open_at (local.get $dir) (i32.const 300) (i32.const 7) (i32.const 5) (i32.const 2))
    (call $expect_err (i32.const 7))
    ;; creating outside the root: `..`, an absolute path, a dangling symlink that leads out
    (call $open_at (local.get $dir) (i32.const 310) (i32.const 23) (i32.const 1) (i32.const 3))
    (call $expect_err (i32.const 31))
    (call $open_at (local.get $dir) (i32.const 340) (i32.const 8) (i32.const 1) (i32.const 3))
    (call $expect_err (i32.const 31))
    (call $open_at (local.get $dir) (i32.const 350) (i32.const 6) (i32.const 1) (i32.const 3))
    (call $expect_err (i32.const 31))
    ;; ... and a new file in a directory that is a symlink out of the root
    (call $open_at (local.get $dir) (i32.const 384) (i32.const 12) (i32.const 1) (i32.const 3))
    (call $expect_err (i32.const 31))
    ;; a name ending in `/` or `/.` names a directory, so it cannot be created as a file
    (call $open_at (local.get $dir) (i32.const 450) (i32.const 5) (i32.const 1) (i32.const 3))
    (call $expect_err (i32.const 12))
    (call $open_at (local.get $dir) (i32.const 460) (i32.const 6) (i32.const 1) (i32.const 3))
    (call $expect_err (i32.const 12))
    ;; a file opened for reading only has no output stream
    (call $open_at (local.get $dir) (i32.const 300) (i32.const 7) (i32.const 0) (i32.const 1))
    (local.set $f (i32.load (i32.const 224)))
    (call $wvs (local.get $f) (i64.const 0) (i32.const 240))
    (if (i32.eqz (i32.load8_u (i32.const 240))) (then unreachable))
    (call $drop_fd (local.get $f)))
"#;

#[cfg(unix)]
#[test]
fn a_writable_preopen_creates_writes_appends_and_truncates_files() {
    let root = std::env::temp_dir().join("wasm2rs_wasip2_write_root");
    std::fs::remove_dir_all(&root).ok();
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::write(root.join("trunc.txt"), "old contents").unwrap();
    let outside = root.parent().unwrap();
    let escape = outside.join("wasm2rs_w_escape.txt");
    std::fs::remove_file(&escape).ok();
    // a symlink under the root to a file outside it that does not exist yet
    let target = outside.join("wasm2rs_w_dangling_target.txt");
    std::fs::remove_file(&target).ok();
    std::os::unix::fs::symlink(&target, root.join("dangle")).unwrap();
    // a symlink under the root to a directory outside it
    let outdir = outside.join("wasm2rs_w_outdir");
    std::fs::remove_dir_all(&outdir).ok();
    std::fs::create_dir_all(&outdir).unwrap();
    std::os::unix::fs::symlink(&outdir, root.join("outdir")).unwrap();
    let bin = compile(
        "fs_write",
        &hello_with(WRITE_IMPORTS, WRITE),
        &preopen_host_with(&root, "with_writable_preopen"),
        "inst.fs_write();",
    );
    run_ok(&bin);
    let read = |name: &str| std::fs::read_to_string(root.join(name)).unwrap();
    assert_eq!(read("new.txt"), "hEYlo world");
    assert_eq!(read("sub/in.txt"), "in");
    assert_eq!(read("trunc.txt"), "N");
    assert!(!escape.exists(), "created a file outside the root");
    assert!(!target.exists(), "wrote through a symlink that leads out");
    assert!(
        !root.join("newd").exists(),
        "created a file for a directory name"
    );
    assert!(
        !outdir.join("x.txt").exists(),
        "created a file through a linked directory"
    );
}

/// The imports and path strings (at 300 to 523) of the tests that create, remove and
/// rename entries. Each of these calls returns `result<_, error-code>`: the tag at
/// `ret`, the error's tag byte at `ret + 1`.
const REMOVE_IMPORTS: &str = r#"
  (import "wasi:filesystem/preopens@0.2.0" "get-directories" (func $dirs (param i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.create-directory-at" (func $mkdir_at (param i32 i32 i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.remove-directory-at" (func $rmdir_at (param i32 i32 i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.unlink-file-at" (func $unlink_at (param i32 i32 i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.rename-at" (func $rename_at (param i32 i32 i32 i32 i32 i32 i32)))
  (data (i32.const 300) "made")
  (data (i32.const 306) "nodir/child")
  (data (i32.const 319) "outdir/evil")
  (data (i32.const 332) "../wasm2rs_b_x")
  (data (i32.const 348) "sub/")
  (data (i32.const 354) "del.txt")
  (data (i32.const 363) "emptydir")
  (data (i32.const 373) "link_out")
  (data (i32.const 383) "outdir/victim.txt")
  (data (i32.const 402) "fulldir")
  (data (i32.const 411) "ren_a.txt")
  (data (i32.const 422) "outdir")
  (data (i32.const 430) "fulldir/")
  (data (i32.const 440) "ren_b.txt")
  (data (i32.const 451) "made/in.txt")
  (data (i32.const 464) "outdir/stolen.txt")
  (data (i32.const 483) "mine.txt")
  (data (i32.const 493) "missing")
  (data (i32.const 502) "x.txt")
  (data (i32.const 509) "linked.txt")
  (data (i32.const 521) "o2")
"#;

/// `fs_remove` runs on a writable root holding `del.txt`, `emptydir/`, `fulldir/f.txt`,
/// `ren_a.txt`, a symlink `link_out` to a file outside the root and a symlink `outdir`
/// to a directory outside it (holding `victim.txt`). Error codes: exist 7, invalid 12,
/// is-directory 14, no-entry 20, not-directory 24, not-empty 25, not-permitted 31,
/// read-only 33. `fs_denied` makes the same calls on a read-only root.
const REMOVE: &str = r#"
  (func $expect (param $code i32)
    (if (i32.eqz (local.get $code))
      (then (if (i32.load8_u (i32.const 220)) (then unreachable)))
      (else
        (if (i32.ne (i32.load8_u (i32.const 220)) (i32.const 1)) (then unreachable))
        (if (i32.ne (i32.load8_u (i32.const 221)) (local.get $code)) (then unreachable)))))
  (func $mkdir (param $dir i32) (param $p i32) (param $n i32) (param $ret i32)
    (call $mkdir_at (local.get $dir) (local.get $p) (local.get $n) (local.get $ret)))
  (func $rmdir (param $dir i32) (param $p i32) (param $n i32) (param $ret i32)
    (call $rmdir_at (local.get $dir) (local.get $p) (local.get $n) (local.get $ret)))
  (func $unlink (param $dir i32) (param $p i32) (param $n i32) (param $ret i32)
    (call $unlink_at (local.get $dir) (local.get $p) (local.get $n) (local.get $ret)))
  (func $rename (param $dir i32) (param $p i32) (param $n i32) (param $to i32) (param $q i32) (param $m i32) (param $ret i32)
    (call $rename_at (local.get $dir) (local.get $p) (local.get $n) (local.get $to) (local.get $q) (local.get $m) (local.get $ret)))
  (func (export "fs_remove") (local $dir i32)
    (call $dirs (i32.const 200))
    (local.set $dir (i32.load (i32.load (i32.const 200))))
    (call $mkdir (local.get $dir) (i32.const 300) (i32.const 4) (i32.const 220))
    (call $expect (i32.const 0))
    (call $mkdir (local.get $dir) (i32.const 300) (i32.const 4) (i32.const 220))
    (call $expect (i32.const 7))
    (call $mkdir (local.get $dir) (i32.const 306) (i32.const 11) (i32.const 220))
    (call $expect (i32.const 20))
    (call $mkdir (local.get $dir) (i32.const 319) (i32.const 11) (i32.const 220))
    (call $expect (i32.const 31))
    (call $mkdir (local.get $dir) (i32.const 332) (i32.const 14) (i32.const 220))
    (call $expect (i32.const 31))
    (call $mkdir (local.get $dir) (i32.const 348) (i32.const 4) (i32.const 220))
    (call $expect (i32.const 12))
    (call $unlink (local.get $dir) (i32.const 354) (i32.const 7) (i32.const 220))
    (call $expect (i32.const 0))
    (call $unlink (local.get $dir) (i32.const 354) (i32.const 7) (i32.const 220))
    (call $expect (i32.const 20))
    (call $unlink (local.get $dir) (i32.const 363) (i32.const 8) (i32.const 220))
    (call $expect (i32.const 14))
    ;; a symlink cannot be renamed: moved onto a name a descriptor holds, it would lead
    ;; that descriptor out of the root
    (call $rename (local.get $dir) (i32.const 373) (i32.const 8) (local.get $dir) (i32.const 509) (i32.const 10) (i32.const 220))
    (call $expect (i32.const 31))
    (call $rename (local.get $dir) (i32.const 422) (i32.const 6) (local.get $dir) (i32.const 521) (i32.const 2) (i32.const 220))
    (call $expect (i32.const 31))
    (call $unlink (local.get $dir) (i32.const 373) (i32.const 8) (i32.const 220))
    (call $expect (i32.const 0))
    (call $unlink (local.get $dir) (i32.const 383) (i32.const 17) (i32.const 220))
    (call $expect (i32.const 31))
    (call $unlink (local.get $dir) (i32.const 332) (i32.const 14) (i32.const 220))
    (call $expect (i32.const 31))
    (call $rmdir (local.get $dir) (i32.const 363) (i32.const 8) (i32.const 220))
    (call $expect (i32.const 0))
    (call $rmdir (local.get $dir) (i32.const 402) (i32.const 7) (i32.const 220))
    (call $expect (i32.const 25))
    (call $rmdir (local.get $dir) (i32.const 411) (i32.const 9) (i32.const 220))
    (call $expect (i32.const 24))
    (call $rmdir (local.get $dir) (i32.const 422) (i32.const 6) (i32.const 220))
    (call $expect (i32.const 24))
    (call $rmdir (local.get $dir) (i32.const 430) (i32.const 8) (i32.const 220))
    (call $expect (i32.const 12))
    (call $rename (local.get $dir) (i32.const 411) (i32.const 9) (local.get $dir) (i32.const 440) (i32.const 9) (i32.const 220))
    (call $expect (i32.const 0))
    (call $rename (local.get $dir) (i32.const 440) (i32.const 9) (local.get $dir) (i32.const 451) (i32.const 11) (i32.const 220))
    (call $expect (i32.const 0))
    (call $rename (local.get $dir) (i32.const 451) (i32.const 11) (local.get $dir) (i32.const 464) (i32.const 17) (i32.const 220))
    (call $expect (i32.const 31))
    (call $rename (local.get $dir) (i32.const 383) (i32.const 17) (local.get $dir) (i32.const 483) (i32.const 8) (i32.const 220))
    (call $expect (i32.const 31))
    (call $rename (local.get $dir) (i32.const 493) (i32.const 7) (local.get $dir) (i32.const 502) (i32.const 5) (i32.const 220))
    (call $expect (i32.const 20)))
  (func (export "fs_denied") (local $dir i32)
    (call $dirs (i32.const 200))
    (local.set $dir (i32.load (i32.load (i32.const 200))))
    (call $mkdir (local.get $dir) (i32.const 300) (i32.const 4) (i32.const 220))
    (call $expect (i32.const 33))
    (call $unlink (local.get $dir) (i32.const 354) (i32.const 7) (i32.const 220))
    (call $expect (i32.const 33))
    (call $rmdir (local.get $dir) (i32.const 363) (i32.const 8) (i32.const 220))
    (call $expect (i32.const 33))
    (call $rename (local.get $dir) (i32.const 411) (i32.const 9) (local.get $dir) (i32.const 502) (i32.const 5) (i32.const 220))
    (call $expect (i32.const 33)))
"#;

/// A root for the remove/rename tests, and the files and directories outside it that
/// they must not touch: `outside/file.txt` (behind `link_out`) and
/// `outside/dir/victim.txt` (behind `outdir`).
#[cfg(unix)]
fn remove_fixture(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!("wasm2rs_wasip2_{name}_root"));
    let outside = std::env::temp_dir().join(format!("wasm2rs_wasip2_{name}_outside"));
    std::fs::remove_dir_all(&root).ok();
    std::fs::remove_dir_all(&outside).ok();
    std::fs::create_dir_all(root.join("emptydir")).unwrap();
    std::fs::create_dir_all(root.join("fulldir")).unwrap();
    std::fs::create_dir_all(outside.join("dir")).unwrap();
    std::fs::write(root.join("del.txt"), "d").unwrap();
    std::fs::write(root.join("fulldir/f.txt"), "f").unwrap();
    std::fs::write(root.join("ren_a.txt"), "moved").unwrap();
    std::fs::write(outside.join("file.txt"), "keep").unwrap();
    std::fs::write(outside.join("dir/victim.txt"), "keep").unwrap();
    std::os::unix::fs::symlink(outside.join("file.txt"), root.join("link_out")).unwrap();
    std::os::unix::fs::symlink(outside.join("dir"), root.join("outdir")).unwrap();
    (root, outside)
}

#[cfg(unix)]
#[test]
fn a_writable_preopen_creates_and_removes_directories_and_renames_entries() {
    let (root, outside) = remove_fixture("remove");
    let bin = compile(
        "fs_remove",
        &hello_with(REMOVE_IMPORTS, REMOVE),
        &preopen_host_with(&root, "with_writable_preopen"),
        "inst.fs_remove();",
    );
    run_ok(&bin);
    assert!(root.join("made").is_dir());
    assert_eq!(
        std::fs::read_to_string(root.join("made/in.txt")).unwrap(),
        "moved"
    );
    for gone in ["del.txt", "emptydir", "ren_a.txt", "ren_b.txt", "link_out"] {
        assert!(
            root.join(gone).symlink_metadata().is_err(),
            "{gone} is still there"
        );
    }
    assert_eq!(
        std::fs::read_to_string(root.join("fulldir/f.txt")).unwrap(),
        "f"
    );
    // nothing outside the root changed, and the link to the directory is still a link
    assert_eq!(
        std::fs::read_to_string(outside.join("file.txt")).unwrap(),
        "keep"
    );
    assert_eq!(
        std::fs::read_to_string(outside.join("dir/victim.txt")).unwrap(),
        "keep"
    );
    assert_eq!(std::fs::read_dir(outside.join("dir")).unwrap().count(), 1);
    assert!(
        root.join("outdir")
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(!root.join("mine.txt").exists());
    assert!(!root.join("linked.txt").exists() && !root.join("o2").exists());
    assert!(!root.parent().unwrap().join("wasm2rs_b_x").exists());
}

#[cfg(unix)]
#[test]
fn a_read_only_preopen_refuses_to_create_remove_and_rename() {
    let (root, outside) = remove_fixture("denied");
    let bin = compile(
        "fs_denied",
        &hello_with(REMOVE_IMPORTS, REMOVE),
        &preopen_host(&root),
        "inst.fs_denied();",
    );
    run_ok(&bin);
    assert!(!root.join("made").exists());
    for kept in ["del.txt", "emptydir", "ren_a.txt"] {
        assert!(root.join(kept).exists(), "{kept} was removed");
    }
    assert!(outside.join("file.txt").exists());
}

/// The imports of the tests that resize, sync and touch files. `set-size` takes the
/// new size (`i64`); `set-times` takes two `new-timestamp`s, each flattened to a tag
/// (no-change 0, now 1, timestamp 2), seconds (`i64`) and nanoseconds. Every call
/// returns `result<_, error-code>`: the tag at `ret`, the error's tag byte at `ret + 1`.
const TIMES_IMPORTS: &str = r#"
  (import "wasi:filesystem/preopens@0.2.0" "get-directories" (func $dirs (param i32)))
  (import "wasi:filesystem/types@0.2.0" "[resource-drop]descriptor" (func $drop_fd (param i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.open-at" (func $open (param i32 i32 i32 i32 i32 i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.set-size" (func $set_size (param i32 i64 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.sync" (func $sync (param i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.sync-data" (func $sync_data (param i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.set-times" (func $set_times (param i32 i32 i64 i32 i32 i64 i32 i32)))
  (data (i32.const 300) "f.txt")
"#;

/// `fs_times` runs on a writable root holding `f.txt` ("0123456789"). open-flags 0
/// opens an existing file; descriptor-flags: read 1, write 2. Error codes:
/// bad-descriptor 3, invalid 12, is-directory 14, read-only 33. `fs_times_denied` runs
/// on a read-only root.
const TIMES: &str = r#"
  (func $ok (param $at i32)
    (if (i32.load8_u (local.get $at)) (then unreachable)))
  (func $err (param $code i32)
    (if (i32.ne (i32.load8_u (i32.const 220)) (i32.const 1)) (then unreachable))
    (if (i32.ne (i32.load8_u (i32.const 221)) (local.get $code)) (then unreachable)))
  (func (export "fs_times") (local $dir i32) (local $f i32) (local $ro i32)
    (call $dirs (i32.const 200))
    (local.set $dir (i32.load (i32.load (i32.const 200))))
    (call $open (local.get $dir) (i32.const 1) (i32.const 300) (i32.const 5) (i32.const 0) (i32.const 3) (i32.const 220))
    (call $ok (i32.const 220))
    (local.set $f (i32.load (i32.const 224)))
    (call $open (local.get $dir) (i32.const 1) (i32.const 300) (i32.const 5) (i32.const 0) (i32.const 1) (i32.const 220))
    (call $ok (i32.const 220))
    (local.set $ro (i32.load (i32.const 224)))
    ;; shrink, then extend (with zeros)
    (call $set_size (local.get $f) (i64.const 4) (i32.const 220))
    (call $ok (i32.const 220))
    (call $set_size (local.get $f) (i64.const 8) (i32.const 220))
    (call $ok (i32.const 220))
    (call $sync (local.get $f) (i32.const 220))
    (call $ok (i32.const 220))
    (call $sync_data (local.get $f) (i32.const 220))
    (call $ok (i32.const 220))
    ;; a file opened for reading only cannot be resized; a directory is not a file
    (call $set_size (local.get $ro) (i64.const 1) (i32.const 220))
    (call $err (i32.const 3))
    (call $set_size (local.get $dir) (i64.const 1) (i32.const 220))
    (call $err (i32.const 14))
    ;; a directory can be synced
    (call $sync (local.get $dir) (i32.const 220))
    (call $ok (i32.const 220))
    ;; both times to now; then no change to the access time and a time for the file
    (call $set_times (local.get $f) (i32.const 1) (i64.const 0) (i32.const 0)
                     (i32.const 1) (i64.const 0) (i32.const 0) (i32.const 220))
    (call $ok (i32.const 220))
    (call $set_times (local.get $f) (i32.const 0) (i64.const 0) (i32.const 0)
                     (i32.const 2) (i64.const 1000000000) (i32.const 500) (i32.const 220))
    (call $ok (i32.const 220))
    ;; a time for the directory
    (call $set_times (local.get $dir) (i32.const 2) (i64.const 2000000000) (i32.const 0)
                     (i32.const 2) (i64.const 2000000000) (i32.const 0) (i32.const 220))
    (call $ok (i32.const 220))
    ;; a file opened for reading only keeps its times
    (call $set_times (local.get $ro) (i32.const 1) (i64.const 0) (i32.const 0)
                     (i32.const 1) (i64.const 0) (i32.const 0) (i32.const 220))
    (call $err (i32.const 3))
    ;; a tag that is none of the three, and seconds that overflow (-1 is 2^64 - 1)
    (call $set_times (local.get $f) (i32.const 3) (i64.const 0) (i32.const 0)
                     (i32.const 0) (i64.const 0) (i32.const 0) (i32.const 220))
    (call $err (i32.const 12))
    (call $set_times (local.get $f) (i32.const 0) (i64.const 0) (i32.const 0)
                     (i32.const 2) (i64.const -1) (i32.const 0) (i32.const 220))
    (call $err (i32.const 12))
    ;; nanoseconds out of range
    (call $set_times (local.get $f) (i32.const 0) (i64.const 0) (i32.const 0)
                     (i32.const 2) (i64.const 5) (i32.const 1000000000) (i32.const 220))
    (call $err (i32.const 12))
    (call $drop_fd (local.get $f))
    (call $drop_fd (local.get $ro)))
  (func (export "fs_times_denied") (local $dir i32) (local $f i32)
    (call $dirs (i32.const 200))
    (local.set $dir (i32.load (i32.load (i32.const 200))))
    (call $open (local.get $dir) (i32.const 1) (i32.const 300) (i32.const 5) (i32.const 0) (i32.const 1) (i32.const 220))
    (call $ok (i32.const 220))
    (local.set $f (i32.load (i32.const 224)))
    (call $set_size (local.get $f) (i64.const 1) (i32.const 220))
    (call $err (i32.const 33))
    (call $set_times (local.get $f) (i32.const 1) (i64.const 0) (i32.const 0)
                     (i32.const 1) (i64.const 0) (i32.const 0) (i32.const 220))
    (call $err (i32.const 33))
    (call $set_times (local.get $dir) (i32.const 1) (i64.const 0) (i32.const 0)
                     (i32.const 1) (i64.const 0) (i32.const 0) (i32.const 220))
    (call $err (i32.const 33))
    ;; syncing changes nothing, so a read-only preopen allows it
    (call $sync (local.get $f) (i32.const 220))
    (call $ok (i32.const 220))
    (call $drop_fd (local.get $f)))
"#;

#[cfg(unix)]
/// A fresh writable-test root `wasm2rs_wasip2_<name>_root` holding `f.txt` ("0123456789").
fn root_with_file(name: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("wasm2rs_wasip2_{name}_root"));
    std::fs::remove_dir_all(&root).ok();
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("f.txt"), "0123456789").unwrap();
    root
}

#[cfg(unix)]
fn epoch_secs(time: std::time::SystemTime) -> u64 {
    time.duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

#[cfg(unix)]
#[test]
fn a_writable_preopen_resizes_syncs_and_sets_the_times_of_files() {
    let root = root_with_file("times");
    let bin = compile(
        "fs_times",
        &hello_with(TIMES_IMPORTS, TIMES),
        &preopen_host_with(&root, "with_writable_preopen"),
        "inst.fs_times();",
    );
    run_ok(&bin);
    assert_eq!(std::fs::read(root.join("f.txt")).unwrap(), b"0123\0\0\0\0");
    let modified = std::fs::metadata(root.join("f.txt"))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(epoch_secs(modified), 1_000_000_000);
    let modified = std::fs::metadata(&root).unwrap().modified().unwrap();
    assert_eq!(epoch_secs(modified), 2_000_000_000);
}

#[cfg(unix)]
#[test]
fn a_read_only_preopen_refuses_to_resize_or_touch_files() {
    let root = root_with_file("times_denied");
    let before = std::fs::metadata(root.join("f.txt"))
        .unwrap()
        .modified()
        .unwrap();
    let bin = compile(
        "fs_times_denied",
        &hello_with(TIMES_IMPORTS, TIMES),
        &preopen_host(&root),
        "inst.fs_times_denied();",
    );
    run_ok(&bin);
    let meta = std::fs::metadata(root.join("f.txt")).unwrap();
    assert_eq!(meta.len(), 10);
    assert_eq!(meta.modified().unwrap(), before);
}

/// The imports and paths of the test of `set-times-at` and `readlink-at`. Both return
/// `result<_, error-code>`-shaped values at `ret` (220): the tag at 220 and, for
/// `readlink-at`, the string's `(ptr, len)` at 224/228 or, for an error, its tag byte at
/// 224 (a string's alignment is 4), while `set-times-at` puts it at 221. Paths: `f.txt`,
/// `g.txt`, `ln` (a symlink to `f.txt`), `abs` (to an absolute path), `up` (to `../x`),
/// `nope` (nothing), `../f.txt`.
const LINK_TIMES_IMPORTS: &str = r#"
  (import "wasi:filesystem/preopens@0.2.0" "get-directories" (func $dirs (param i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.set-times-at" (func $set_times_at (param i32 i32 i32 i32 i32 i64 i32 i32 i64 i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.readlink-at" (func $readlink_at (param i32 i32 i32 i32)))
  (data (i32.const 300) "f.txt")
  (data (i32.const 310) "g.txt")
  (data (i32.const 320) "ln")
  (data (i32.const 330) "abs")
  (data (i32.const 340) "up")
  (data (i32.const 350) "nope")
  (data (i32.const 360) "../f.txt")
"#;

/// `fs_links` runs on a writable root, `fs_links_denied` on a read-only one. Error codes:
/// invalid 12, no-entry 20, unsupported 27, not-permitted 31, read-only 33. Path-flags:
/// symlink-follow 1.
const LINK_TIMES: &str = r#"
  (func $ok (param $at i32)
    (if (i32.load8_u (local.get $at)) (then unreachable)))
  (func $err (param $at i32) (param $code i32)
    (if (i32.ne (i32.load8_u (i32.const 220)) (i32.const 1)) (then unreachable))
    (if (i32.ne (i32.load8_u (local.get $at)) (local.get $code)) (then unreachable)))
  ;; the modification time of `path` set to `secs` (nanoseconds `nanos`), the access time kept
  (func $touch (param $dir i32) (param $flags i32) (param $path i32) (param $len i32)
               (param $secs i64) (param $nanos i32)
    (call $set_times_at (local.get $dir) (local.get $flags) (local.get $path) (local.get $len)
                        (i32.const 0) (i64.const 0) (i32.const 0)
                        (i32.const 2) (local.get $secs) (local.get $nanos) (i32.const 220)))
  ;; `readlink-at` of `path` is the `want_len` bytes at `want`
  (func $target_is (param $dir i32) (param $path i32) (param $len i32)
                   (param $want i32) (param $want_len i32) (local $i i32)
    (call $readlink_at (local.get $dir) (local.get $path) (local.get $len) (i32.const 220))
    (call $ok (i32.const 220))
    (if (i32.ne (i32.load (i32.const 228)) (local.get $want_len)) (then unreachable))
    (loop $next
      (if (i32.ne (i32.load8_u (i32.add (i32.load (i32.const 224)) (local.get $i)))
                  (i32.load8_u (i32.add (local.get $want) (local.get $i))))
        (then unreachable))
      (local.set $i (i32.add (local.get $i) (i32.const 1)))
      (br_if $next (i32.lt_u (local.get $i) (local.get $want_len)))))
  (func (export "fs_links") (local $dir i32)
    (call $dirs (i32.const 200))
    (local.set $dir (i32.load (i32.load (i32.const 200))))
    ;; the target of a relative symlink, as the link holds it
    (call $target_is (local.get $dir) (i32.const 320) (i32.const 2) (i32.const 300) (i32.const 5))
    ;; a file is no symlink; a target that leaves the preopen is not shown
    (call $readlink_at (local.get $dir) (i32.const 300) (i32.const 5) (i32.const 220))
    (call $err (i32.const 224) (i32.const 12))
    (call $readlink_at (local.get $dir) (i32.const 330) (i32.const 3) (i32.const 220))
    (call $err (i32.const 224) (i32.const 31))
    (call $readlink_at (local.get $dir) (i32.const 340) (i32.const 2) (i32.const 220))
    (call $err (i32.const 224) (i32.const 31))
    (call $readlink_at (local.get $dir) (i32.const 350) (i32.const 4) (i32.const 220))
    (call $err (i32.const 224) (i32.const 20))
    (call $readlink_at (local.get $dir) (i32.const 360) (i32.const 8) (i32.const 220))
    (call $err (i32.const 224) (i32.const 31))
    ;; following the symlink sets the times of its target
    (call $touch (local.get $dir) (i32.const 1) (i32.const 320) (i32.const 2) (i64.const 1000000000) (i32.const 0))
    (call $ok (i32.const 220))
    ;; now for both, on g.txt (its modification time is set below)
    (call $set_times_at (local.get $dir) (i32.const 1) (i32.const 310) (i32.const 5)
                        (i32.const 1) (i64.const 0) (i32.const 0)
                        (i32.const 1) (i64.const 0) (i32.const 0) (i32.const 220))
    (call $ok (i32.const 220))
    ;; without following, a file is set as it is; a symlink cannot be
    (call $touch (local.get $dir) (i32.const 0) (i32.const 310) (i32.const 5) (i64.const 1500000000) (i32.const 0))
    (call $ok (i32.const 220))
    (call $touch (local.get $dir) (i32.const 0) (i32.const 320) (i32.const 2) (i64.const 1) (i32.const 0))
    (call $err (i32.const 221) (i32.const 27))
    ;; out-of-range nanoseconds, a missing path, and paths outside the preopen
    (call $touch (local.get $dir) (i32.const 1) (i32.const 300) (i32.const 5) (i64.const 5) (i32.const 1000000000))
    (call $err (i32.const 221) (i32.const 12))
    (call $touch (local.get $dir) (i32.const 1) (i32.const 350) (i32.const 4) (i64.const 5) (i32.const 0))
    (call $err (i32.const 221) (i32.const 20))
    (call $touch (local.get $dir) (i32.const 1) (i32.const 360) (i32.const 8) (i64.const 5) (i32.const 0))
    (call $err (i32.const 221) (i32.const 31))
    ;; a symlink that leads out is refused when followed (`abs`), or has nothing at its target (`up`)
    (call $touch (local.get $dir) (i32.const 1) (i32.const 330) (i32.const 3) (i64.const 5) (i32.const 0))
    (call $err (i32.const 221) (i32.const 31))
    (call $touch (local.get $dir) (i32.const 1) (i32.const 340) (i32.const 2) (i64.const 5) (i32.const 0))
    (call $err (i32.const 221) (i32.const 20)))
  (func (export "fs_links_denied") (local $dir i32)
    (call $dirs (i32.const 200))
    (local.set $dir (i32.load (i32.load (i32.const 200))))
    (call $touch (local.get $dir) (i32.const 1) (i32.const 300) (i32.const 5) (i64.const 5) (i32.const 0))
    (call $err (i32.const 221) (i32.const 33))
    ;; reading a link changes nothing
    (call $target_is (local.get $dir) (i32.const 320) (i32.const 2) (i32.const 300) (i32.const 5)))
"#;

/// A fresh root `wasm2rs_wasip2_<name>_root` of `root_with_file`, plus `g.txt` and the
/// symlinks `ln` (to `f.txt`), `abs` (to an absolute path) and `up` (to `../x`).
#[cfg(unix)]
fn root_with_links(name: &str) -> std::path::PathBuf {
    let root = root_with_file(name);
    std::fs::write(root.join("g.txt"), "g").unwrap();
    std::os::unix::fs::symlink("f.txt", root.join("ln")).unwrap();
    std::os::unix::fs::symlink(std::env::temp_dir(), root.join("abs")).unwrap();
    std::os::unix::fs::symlink("../x", root.join("up")).unwrap();
    root
}

#[cfg(unix)]
#[test]
fn set_times_at_follows_symlinks_on_request_and_readlink_at_shows_relative_targets() {
    let root = root_with_links("links");
    let bin = compile(
        "fs_links",
        &hello_with(LINK_TIMES_IMPORTS, LINK_TIMES),
        &preopen_host_with(&root, "with_writable_preopen"),
        "inst.fs_links();",
    );
    run_ok(&bin);
    let modified = |name: &str| {
        std::fs::metadata(root.join(name))
            .unwrap()
            .modified()
            .unwrap()
    };
    assert_eq!(epoch_secs(modified("f.txt")), 1_000_000_000);
    // g.txt's times were set to now, then its modification time to 1_500_000_000
    assert_eq!(epoch_secs(modified("g.txt")), 1_500_000_000);
    let accessed = std::fs::metadata(root.join("g.txt"))
        .unwrap()
        .accessed()
        .unwrap();
    assert!(epoch_secs(accessed) > 1_600_000_000);
    // the host directory `abs` leads to was not touched
    let outside = std::fs::metadata(std::env::temp_dir())
        .unwrap()
        .modified()
        .unwrap();
    assert_ne!(epoch_secs(outside), 5);
}

#[cfg(unix)]
#[test]
fn a_read_only_preopen_refuses_set_times_at_but_shows_links() {
    let root = root_with_links("links_denied");
    let before = std::fs::metadata(root.join("f.txt"))
        .unwrap()
        .modified()
        .unwrap();
    let bin = compile(
        "fs_links_denied",
        &hello_with(LINK_TIMES_IMPORTS, LINK_TIMES),
        &preopen_host(&root),
        "inst.fs_links_denied();",
    );
    run_ok(&bin);
    assert_eq!(
        std::fs::metadata(root.join("f.txt"))
            .unwrap()
            .modified()
            .unwrap(),
        before
    );
}

/// The imports and paths of the test of `symlink-at` and `link-at`. Both return
/// `result<_, error-code>`: the tag at 220, the error's tag byte at 221.
const MAKE_LINKS_IMPORTS: &str = r#"
  (import "wasi:filesystem/preopens@0.2.0" "get-directories" (func $dirs (param i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.symlink-at" (func $symlink_at (param i32 i32 i32 i32 i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.link-at" (func $link_at (param i32 i32 i32 i32 i32 i32 i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.readlink-at" (func $readlink_at (param i32 i32 i32 i32)))
  (data (i32.const 300) "f.txt")
  (data (i32.const 310) "ln")
  (data (i32.const 320) "new_ln")
  (data (i32.const 330) "s2")
  (data (i32.const 340) "../x")
  (data (i32.const 350) "/etc")
  (data (i32.const 360) "../h")
  (data (i32.const 370) "h.txt")
  (data (i32.const 380) "h2.txt")
  (data (i32.const 390) "h3")
  (data (i32.const 400) "h4")
  (data (i32.const 410) "d")
  (data (i32.const 420) "../f.txt")
  (data (i32.const 430) "nope")
  (data (i32.const 440) "abs")
  (data (i32.const 450) "up")
"#;

/// `fs_make_links` runs on a writable root holding `f.txt`, `g.txt`, `ln` (a symlink to
/// `f.txt`) and the directory `d`; `fs_make_links_denied` on a read-only one. Error
/// codes: exist 7, no-entry 20, unsupported 27, not-permitted 31, read-only 33.
/// Path-flags: symlink-follow 1.
const MAKE_LINKS: &str = r#"
  (func $ok
    (if (i32.load8_u (i32.const 220)) (then unreachable)))
  (func $err (param $code i32)
    (if (i32.ne (i32.load8_u (i32.const 220)) (i32.const 1)) (then unreachable))
    (if (i32.ne (i32.load8_u (i32.const 221)) (local.get $code)) (then unreachable)))
  ;; `symlink-at` of the target at (old, old_len) as the name at (new, new_len)
  (func $sym (param $dir i32) (param $old i32) (param $old_len i32) (param $new i32) (param $new_len i32)
    (call $symlink_at (local.get $dir) (local.get $old) (local.get $old_len)
                      (local.get $new) (local.get $new_len) (i32.const 220)))
  ;; `link-at` of the path at (old, old_len) as the name at (new, new_len)
  (func $hard (param $dir i32) (param $flags i32) (param $old i32) (param $old_len i32) (param $new i32) (param $new_len i32)
    (call $link_at (local.get $dir) (local.get $flags) (local.get $old) (local.get $old_len)
                   (local.get $dir) (local.get $new) (local.get $new_len) (i32.const 220)))
  (func (export "fs_make_links") (local $dir i32)
    (call $dirs (i32.const 200))
    (local.set $dir (i32.load (i32.load (i32.const 200))))
    ;; a symlink to `f.txt`, which `readlink-at` shows as written
    (call $sym (local.get $dir) (i32.const 300) (i32.const 5) (i32.const 320) (i32.const 6))
    (call $ok)
    (call $readlink_at (local.get $dir) (i32.const 320) (i32.const 6) (i32.const 224))
    (if (i32.load8_u (i32.const 224)) (then unreachable))
    (if (i32.ne (i32.load (i32.const 232)) (i32.const 5)) (then unreachable))
    ;; a target that is absolute or has `..`, an empty target, a taken name, a name outside
    (call $sym (local.get $dir) (i32.const 340) (i32.const 4) (i32.const 330) (i32.const 2))
    (call $err (i32.const 31))
    (call $sym (local.get $dir) (i32.const 350) (i32.const 4) (i32.const 330) (i32.const 2))
    (call $err (i32.const 31))
    (call $sym (local.get $dir) (i32.const 300) (i32.const 0) (i32.const 330) (i32.const 2))
    (call $err (i32.const 20))
    (call $sym (local.get $dir) (i32.const 300) (i32.const 5) (i32.const 320) (i32.const 6))
    (call $err (i32.const 7))
    (call $sym (local.get $dir) (i32.const 300) (i32.const 5) (i32.const 360) (i32.const 4))
    (call $err (i32.const 31))
    ;; a hard link to a file, and through a symlink when following it
    (call $hard (local.get $dir) (i32.const 0) (i32.const 300) (i32.const 5) (i32.const 370) (i32.const 5))
    (call $ok)
    (call $hard (local.get $dir) (i32.const 1) (i32.const 310) (i32.const 2) (i32.const 380) (i32.const 6))
    (call $ok)
    ;; not a symlink itself, not a directory, not over a name, not from or to outside, not nothing
    (call $hard (local.get $dir) (i32.const 0) (i32.const 310) (i32.const 2) (i32.const 390) (i32.const 2))
    (call $err (i32.const 27))
    (call $hard (local.get $dir) (i32.const 0) (i32.const 410) (i32.const 1) (i32.const 400) (i32.const 2))
    (call $err (i32.const 31))
    (call $hard (local.get $dir) (i32.const 0) (i32.const 300) (i32.const 5) (i32.const 370) (i32.const 5))
    (call $err (i32.const 7))
    (call $hard (local.get $dir) (i32.const 0) (i32.const 300) (i32.const 5) (i32.const 360) (i32.const 4))
    (call $err (i32.const 31))
    (call $hard (local.get $dir) (i32.const 0) (i32.const 420) (i32.const 8) (i32.const 400) (i32.const 2))
    (call $err (i32.const 31))
    (call $hard (local.get $dir) (i32.const 0) (i32.const 430) (i32.const 4) (i32.const 400) (i32.const 2))
    (call $err (i32.const 20))
    ;; following a symlink that leads out of the preopen, or to nothing
    (call $hard (local.get $dir) (i32.const 1) (i32.const 440) (i32.const 3) (i32.const 400) (i32.const 2))
    (call $err (i32.const 31))
    (call $hard (local.get $dir) (i32.const 1) (i32.const 450) (i32.const 2) (i32.const 400) (i32.const 2))
    (call $err (i32.const 20)))
  (func (export "fs_make_links_denied") (local $dir i32)
    (call $dirs (i32.const 200))
    (local.set $dir (i32.load (i32.load (i32.const 200))))
    (call $sym (local.get $dir) (i32.const 300) (i32.const 5) (i32.const 320) (i32.const 6))
    (call $err (i32.const 33))
    (call $hard (local.get $dir) (i32.const 0) (i32.const 300) (i32.const 5) (i32.const 370) (i32.const 5))
    (call $err (i32.const 33)))
"#;

#[cfg(unix)]
#[test]
fn a_writable_preopen_creates_symlinks_and_hard_links_under_it() {
    use std::os::unix::fs::MetadataExt;
    let root = root_with_links("make_links");
    std::fs::create_dir(root.join("d")).unwrap();
    let bin = compile(
        "fs_make_links",
        &hello_with(MAKE_LINKS_IMPORTS, MAKE_LINKS),
        &preopen_host_with(&root, "with_writable_preopen"),
        "inst.fs_make_links();",
    );
    run_ok(&bin);
    assert_eq!(
        std::fs::read_link(root.join("new_ln")).unwrap(),
        std::path::Path::new("f.txt")
    );
    let ino = |name: &str| std::fs::metadata(root.join(name)).unwrap().ino();
    assert_eq!(ino("h.txt"), ino("f.txt"));
    assert_eq!(ino("h2.txt"), ino("f.txt"));
    for refused in ["s2", "h3", "h4"] {
        assert!(root.join(refused).symlink_metadata().is_err(), "{refused}");
    }
    assert!(!root.parent().unwrap().join("h").exists());
}

#[cfg(unix)]
#[test]
fn a_read_only_preopen_refuses_symlink_at_and_link_at() {
    let root = root_with_links("make_links_denied");
    let bin = compile(
        "fs_make_links_denied",
        &hello_with(MAKE_LINKS_IMPORTS, MAKE_LINKS),
        &preopen_host(&root),
        "inst.fs_make_links_denied();",
    );
    run_ok(&bin);
    for refused in ["new_ln", "h.txt"] {
        assert!(root.join(refused).symlink_metadata().is_err(), "{refused}");
    }
}

/// The imports and paths of the test of `link-at` into another directory descriptor.
const LINK_ACROSS_IMPORTS: &str = r#"
  (import "wasi:filesystem/preopens@0.2.0" "get-directories" (func $dirs (param i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.open-at" (func $open (param i32 i32 i32 i32 i32 i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.link-at" (func $link_at (param i32 i32 i32 i32 i32 i32 i32 i32)))
  (data (i32.const 300) "d")
  (data (i32.const 310) "f.txt")
  (data (i32.const 320) "in_d")
  (data (i32.const 330) "../up")
  (data (i32.const 340) "/abs")
  (data (i32.const 350) "back")
"#;

/// `fs_link_across` runs on a writable root holding `f.txt` and the directory `d`. It
/// opens `d` and links `f.txt` into it (as `in_d`), then `d/in_d` back into the root
/// (as `back`); a new name that is absolute or has `..` is not-permitted (31).
const LINK_ACROSS: &str = r#"
  (func $ok
    (if (i32.load8_u (i32.const 220)) (then unreachable)))
  (func $denied
    (if (i32.ne (i32.load8_u (i32.const 220)) (i32.const 1)) (then unreachable))
    (if (i32.ne (i32.load8_u (i32.const 221)) (i32.const 31)) (then unreachable)))
  (func (export "fs_link_across") (local $dir i32) (local $sub i32)
    (call $dirs (i32.const 200))
    (local.set $dir (i32.load (i32.load (i32.const 200))))
    (call $open (local.get $dir) (i32.const 1) (i32.const 300) (i32.const 1) (i32.const 2) (i32.const 1) (i32.const 220))
    (call $ok)
    (local.set $sub (i32.load (i32.const 224)))
    ;; root/f.txt -> d/in_d
    (call $link_at (local.get $dir) (i32.const 0) (i32.const 310) (i32.const 5)
                   (local.get $sub) (i32.const 320) (i32.const 4) (i32.const 220))
    (call $ok)
    ;; d/in_d -> root/back
    (call $link_at (local.get $sub) (i32.const 0) (i32.const 320) (i32.const 4)
                   (local.get $dir) (i32.const 350) (i32.const 4) (i32.const 220))
    (call $ok)
    ;; a new name that leaves the new directory
    (call $link_at (local.get $dir) (i32.const 0) (i32.const 310) (i32.const 5)
                   (local.get $sub) (i32.const 330) (i32.const 5) (i32.const 220))
    (call $denied)
    (call $link_at (local.get $dir) (i32.const 0) (i32.const 310) (i32.const 5)
                   (local.get $sub) (i32.const 340) (i32.const 4) (i32.const 220))
    (call $denied))
"#;

#[cfg(unix)]
#[test]
fn link_at_into_another_directory_descriptor_links_there() {
    use std::os::unix::fs::MetadataExt;
    let root = root_with_file("link_across");
    std::fs::create_dir(root.join("d")).unwrap();
    let bin = compile(
        "fs_link_across",
        &hello_with(LINK_ACROSS_IMPORTS, LINK_ACROSS),
        &preopen_host_with(&root, "with_writable_preopen"),
        "inst.fs_link_across();",
    );
    run_ok(&bin);
    let ino = |path: std::path::PathBuf| std::fs::metadata(path).unwrap().ino();
    assert_eq!(ino(root.join("d/in_d")), ino(root.join("f.txt")));
    assert_eq!(ino(root.join("back")), ino(root.join("f.txt")));
    assert!(!root.join("in_d").exists());
    assert!(!root.parent().unwrap().join("up").exists());
}

/// The imports and paths of the test where a guest moves a directory under the path of a
/// directory descriptor it holds. Calls that return `result<_, error-code>` put the tag
/// at `ret` and the error's tag byte at `ret + 1`.
const MOVED_IMPORTS: &str = r#"
  (import "wasi:filesystem/preopens@0.2.0" "get-directories" (func $dirs (param i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.open-at" (func $open (param i32 i32 i32 i32 i32 i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.create-directory-at" (func $mkdir (param i32 i32 i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.remove-directory-at" (func $rmdir (param i32 i32 i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.rename-at" (func $rename (param i32 i32 i32 i32 i32 i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.sync" (func $sync (param i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.set-times" (func $set_times (param i32 i32 i64 i32 i32 i64 i32 i32)))
  (data (i32.const 300) "d")
  (data (i32.const 310) "d/sub")
  (data (i32.const 320) "e")
"#;

/// `fs_moved` runs on a writable root holding `e/sub`, a symlink to a directory outside
/// the root. It opens the directory `d/sub`, then makes `d` the directory `e`, so the
/// path the descriptor holds now leads outside the root. Error code not-permitted: 31.
const MOVED: &str = r#"
  (func $ok (param $at i32)
    (if (i32.load8_u (local.get $at)) (then unreachable)))
  (func $denied
    (if (i32.ne (i32.load8_u (i32.const 220)) (i32.const 1)) (then unreachable))
    (if (i32.ne (i32.load8_u (i32.const 221)) (i32.const 31)) (then unreachable)))
  (func (export "fs_moved") (local $dir i32) (local $sub i32)
    (call $dirs (i32.const 200))
    (local.set $dir (i32.load (i32.load (i32.const 200))))
    (call $mkdir (local.get $dir) (i32.const 300) (i32.const 1) (i32.const 220))
    (call $ok (i32.const 220))
    (call $mkdir (local.get $dir) (i32.const 310) (i32.const 5) (i32.const 220))
    (call $ok (i32.const 220))
    (call $open (local.get $dir) (i32.const 1) (i32.const 310) (i32.const 5) (i32.const 2) (i32.const 1) (i32.const 220))
    (call $ok (i32.const 220))
    (local.set $sub (i32.load (i32.const 224)))
    (call $rmdir (local.get $dir) (i32.const 310) (i32.const 5) (i32.const 220))
    (call $ok (i32.const 220))
    (call $rmdir (local.get $dir) (i32.const 300) (i32.const 1) (i32.const 220))
    (call $ok (i32.const 220))
    (call $rename (local.get $dir) (i32.const 320) (i32.const 1) (local.get $dir) (i32.const 300) (i32.const 1) (i32.const 220))
    (call $ok (i32.const 220))
    (call $sync (local.get $sub) (i32.const 220))
    (call $denied)
    (call $set_times (local.get $sub) (i32.const 2) (i64.const 1000000000) (i32.const 0)
                     (i32.const 2) (i64.const 1000000000) (i32.const 0) (i32.const 220))
    (call $denied))
"#;

#[cfg(unix)]
#[test]
fn a_directory_descriptor_whose_path_was_moved_out_of_the_preopen_is_refused() {
    let root = std::env::temp_dir().join("wasm2rs_wasip2_moved_root");
    std::fs::remove_dir_all(&root).ok();
    std::fs::create_dir_all(root.join("e")).unwrap();
    let outside = root.parent().unwrap().join("wasm2rs_wasip2_moved_outside");
    std::fs::remove_dir_all(&outside).ok();
    std::fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, root.join("e/sub")).unwrap();
    let before = std::fs::metadata(&outside).unwrap().modified().unwrap();
    let bin = compile(
        "fs_moved",
        &hello_with(MOVED_IMPORTS, MOVED),
        &preopen_host_with(&root, "with_writable_preopen"),
        "inst.fs_moved();",
    );
    run_ok(&bin);
    let after = std::fs::metadata(&outside).unwrap().modified().unwrap();
    assert_eq!(after, before, "changed a directory outside the root");
}

/// The imports of the tests where a path is reused after a descriptor was opened on it.
const REUSED_IMPORTS: &str = r#"
  (import "wasi:filesystem/preopens@0.2.0" "get-directories" (func $dirs (param i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.open-at" (func $open (param i32 i32 i32 i32 i32 i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.unlink-file-at" (func $unlink (param i32 i32 i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.create-directory-at" (func $mkdir (param i32 i32 i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.set-size" (func $set_size (param i32 i64 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.metadata-hash" (func $hash (param i32 i32)))
  (data (i32.const 300) "f.txt")
  (data (i32.const 310) "x")
  (data (i32.const 320) "w.txt")
"#;

/// `fs_reused` runs on a writable root holding `f.txt` and `w.txt` (write-only). It
/// opens `f.txt`, removes it, creates another file under the same name, then resizes and
/// hashes the first one: a descriptor stays on the file it opened, not on whatever the
/// path names now. open-flags: create 1; descriptor-flags: read 1, write 2.
const REUSED: &str = r#"
  (func $ok (param $at i32)
    (if (i32.load8_u (local.get $at)) (then unreachable)))
  (func (export "fs_reused") (local $dir i32) (local $old i32) (local $new i32) (local $h i64)
    (call $dirs (i32.const 200))
    (local.set $dir (i32.load (i32.load (i32.const 200))))
    (call $open (local.get $dir) (i32.const 1) (i32.const 300) (i32.const 5) (i32.const 0) (i32.const 3) (i32.const 220))
    (call $ok (i32.const 220))
    (local.set $old (i32.load (i32.const 224)))
    (call $unlink (local.get $dir) (i32.const 300) (i32.const 5) (i32.const 220))
    (call $ok (i32.const 220))
    (call $open (local.get $dir) (i32.const 1) (i32.const 300) (i32.const 5) (i32.const 1) (i32.const 3) (i32.const 220))
    (call $ok (i32.const 220))
    (local.set $new (i32.load (i32.const 224)))
    (call $set_size (local.get $old) (i64.const 5) (i32.const 220))
    (call $ok (i32.const 220))
    ;; two files are not the same object, though they were opened under one path
    (call $hash (local.get $old) (i32.const 240))
    (call $ok (i32.const 240))
    (local.set $h (i64.load (i32.const 248)))
    (call $hash (local.get $new) (i32.const 240))
    (call $ok (i32.const 240))
    (if (i64.eq (local.get $h) (i64.load (i32.const 248))) (then unreachable))
    ;; a descriptor for writing only opens a file that cannot be read
    (call $open (local.get $dir) (i32.const 1) (i32.const 320) (i32.const 5) (i32.const 0) (i32.const 2) (i32.const 220))
    (call $ok (i32.const 220)))
  ;; a file descriptor is not a directory, even when a directory now has its path
  (func (export "fs_reused_dir") (local $dir i32) (local $old i32)
    (call $dirs (i32.const 200))
    (local.set $dir (i32.load (i32.load (i32.const 200))))
    (call $open (local.get $dir) (i32.const 1) (i32.const 300) (i32.const 5) (i32.const 0) (i32.const 3) (i32.const 220))
    (call $ok (i32.const 220))
    (local.set $old (i32.load (i32.const 224)))
    (call $unlink (local.get $dir) (i32.const 300) (i32.const 5) (i32.const 220))
    (call $ok (i32.const 220))
    (call $mkdir (local.get $dir) (i32.const 300) (i32.const 5) (i32.const 220))
    (call $ok (i32.const 220))
    (call $open (local.get $old) (i32.const 1) (i32.const 310) (i32.const 1) (i32.const 0) (i32.const 1) (i32.const 220))
    (if (i32.ne (i32.load8_u (i32.const 220)) (i32.const 1)) (then unreachable))
    (if (i32.ne (i32.load8_u (i32.const 224)) (i32.const 24)) (then unreachable)))
"#;

#[cfg(unix)]
#[test]
fn a_descriptor_stays_on_the_file_it_opened_when_its_path_is_reused() {
    let root = root_with_file("reused");
    let write_only = root.join("w.txt");
    std::fs::write(&write_only, "w").unwrap();
    std::fs::set_permissions(
        &write_only,
        std::os::unix::fs::PermissionsExt::from_mode(0o200),
    )
    .unwrap();
    let bin = compile(
        "fs_reused",
        &hello_with(REUSED_IMPORTS, REUSED),
        &preopen_host_with(&root, "with_writable_preopen"),
        "inst.fs_reused();",
    );
    run_ok(&bin);
    let len = std::fs::metadata(root.join("f.txt")).unwrap().len();
    assert_eq!(len, 0, "resized the new file instead of the one opened");
}

#[cfg(unix)]
#[test]
fn a_file_descriptor_is_not_a_directory_when_a_directory_takes_its_path() {
    let root = root_with_file("reused_dir");
    let bin = compile(
        "fs_reused_dir",
        &hello_with(REUSED_IMPORTS, REUSED),
        &preopen_host_with(&root, "with_writable_preopen"),
        "inst.fs_reused_dir();",
    );
    run_ok(&bin);
}

/// The imports of the tests of the streams over one descriptor. `read` returns
/// `result<list<u8>, stream-error>` (tag at `ret`, list pointer at `ret + 4`, length at
/// `ret + 8`); `stat` returns the size at `ret + 24`.
const STREAMS_IMPORTS: &str = r#"
  (import "wasi:filesystem/preopens@0.2.0" "get-directories" (func $dirs (param i32)))
  (import "wasi:filesystem/types@0.2.0" "[resource-drop]descriptor" (func $drop_fd (param i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.open-at" (func $open (param i32 i32 i32 i32 i32 i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.read-via-stream" (func $rvs (param i32 i64 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.write-via-stream" (func $wvs (param i32 i64 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.append-via-stream" (func $avs (param i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.rename-at" (func $rename (param i32 i32 i32 i32 i32 i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.stat" (func $stat (param i32 i32)))
  (import "wasi:io/streams@0.2.0" "[method]input-stream.read" (func $read (param i32 i64 i32)))
  (data (i32.const 300) "f.txt")
  (data (i32.const 310) "g.txt")
  (data (i32.const 500) "abCDefGHZZ")
"#;

/// `fs_streams` runs on a writable root holding `f.txt` ("0123456789"); `fs_renamed`
/// also on `g.txt` (14 bytes). open-flags 0 opens an existing file; descriptor-flags:
/// read 1, write 2. The 2-byte values are little-endian pairs of ASCII: "01" is 0x3130.
const STREAMS: &str = r#"
  (func $ok (param $at i32)
    (if (i32.load8_u (local.get $at)) (then unreachable)))
  ;; the stream behind the handle `result<handle, _>` just put at 244
  (func $handle (result i32)
    (call $ok (i32.const 240))
    (i32.load (i32.const 244)))
  ;; reads 2 bytes from an input stream
  (func $rd (param $s i32) (result i32)
    (call $read (local.get $s) (i64.const 2) (i32.const 260))
    (call $ok (i32.const 260))
    (if (i32.ne (i32.load (i32.const 268)) (i32.const 2)) (then unreachable))
    (i32.load16_u (i32.load (i32.const 264))))
  ;; writes `len` bytes from `ptr` to an output stream
  (func $put (param $s i32) (param $ptr i32) (param $len i32)
    (call $write (local.get $s) (local.get $ptr) (local.get $len) (i32.const 280))
    (call $ok (i32.const 280)))
  (func (export "fs_streams") (local $dir i32) (local $f i32) (local $a i32) (local $b i32)
                             (local $w i32) (local $p i32) (local $r i32) (local $r2 i32) (local $n i32)
    (call $dirs (i32.const 200))
    (local.set $dir (i32.load (i32.load (i32.const 200))))
    (call $open (local.get $dir) (i32.const 1) (i32.const 300) (i32.const 5) (i32.const 0) (i32.const 3) (i32.const 220))
    (call $ok (i32.const 220))
    (local.set $f (i32.load (i32.const 224)))
    ;; two input streams on one descriptor keep their own positions
    (call $rvs (local.get $f) (i64.const 0) (i32.const 240))
    (local.set $a (call $handle))
    (call $rvs (local.get $f) (i64.const 5) (i32.const 240))
    (local.set $b (call $handle))
    (if (i32.ne (call $rd (local.get $a)) (i32.const 0x3130)) (then unreachable))
    (if (i32.ne (call $rd (local.get $b)) (i32.const 0x3635)) (then unreachable))
    (if (i32.ne (call $rd (local.get $a)) (i32.const 0x3332)) (then unreachable))
    (if (i32.ne (call $rd (local.get $b)) (i32.const 0x3837)) (then unreachable))
    ;; a stream writing from offset 2 and an appending one, used in turn
    (call $wvs (local.get $f) (i64.const 2) (i32.const 240))
    (local.set $w (call $handle))
    (call $avs (local.get $f) (i32.const 240))
    (local.set $p (call $handle))
    (call $put (local.get $w) (i32.const 500) (i32.const 2))
    (call $put (local.get $p) (i32.const 502) (i32.const 2))
    (call $put (local.get $w) (i32.const 504) (i32.const 2))
    (call $put (local.get $p) (i32.const 506) (i32.const 2))
    ;; another stream sees all of it: 01abef6789CDGH
    (call $rvs (local.get $f) (i64.const 0) (i32.const 240))
    (local.set $r (call $handle))
    (call $read (local.get $r) (i64.const 100) (i32.const 260))
    (call $ok (i32.const 260))
    (local.set $n (i32.load (i32.const 268)))
    (if (i32.ne (local.get $n) (i32.const 14)) (then unreachable))
    (if (i32.ne (i32.load16_u offset=2 (i32.load (i32.const 264))) (i32.const 0x6261)) (then unreachable))
    (if (i32.ne (i32.load16_u offset=12 (i32.load (i32.const 264))) (i32.const 0x4847)) (then unreachable))
    ;; the streams outlive the descriptor
    (call $rvs (local.get $f) (i64.const 6) (i32.const 240))
    (local.set $r2 (call $handle))
    (call $drop_fd (local.get $f))
    (call $put (local.get $w) (i32.const 508) (i32.const 2))
    (if (i32.ne (call $rd (local.get $r2)) (i32.const 0x5a5a)) (then unreachable)))
  (func (export "fs_renamed") (local $dir i32) (local $old i32)
    (call $dirs (i32.const 200))
    (local.set $dir (i32.load (i32.load (i32.const 200))))
    (call $open (local.get $dir) (i32.const 1) (i32.const 300) (i32.const 5) (i32.const 0) (i32.const 1) (i32.const 220))
    (call $ok (i32.const 220))
    (local.set $old (i32.load (i32.const 224)))
    ;; another file takes the name `f.txt`
    (call $rename (local.get $dir) (i32.const 310) (i32.const 5) (local.get $dir) (i32.const 300) (i32.const 5) (i32.const 220))
    (call $ok (i32.const 220))
    ;; the descriptor still has the 10-byte file it opened, not the 14-byte one
    (call $stat (local.get $old) (i32.const 600))
    (call $ok (i32.const 600))
    (if (i64.ne (i64.load (i32.const 624)) (i64.const 10)) (then unreachable))
    (call $rvs (local.get $old) (i64.const 0) (i32.const 240))
    (if (i32.ne (call $rd (call $handle)) (i32.const 0x3130)) (then unreachable)))
"#;

#[cfg(unix)]
#[test]
fn streams_over_one_descriptor_keep_their_own_positions_and_outlive_it() {
    let root = root_with_file("streams");
    let bin = compile(
        "fs_streams",
        &hello_with(STREAMS_IMPORTS, STREAMS),
        &preopen_host_with(&root, "with_writable_preopen"),
        "inst.fs_streams();",
    );
    run_ok(&bin);
    assert_eq!(
        std::fs::read_to_string(root.join("f.txt")).unwrap(),
        "01abefZZ89CDGH"
    );
}

#[cfg(unix)]
#[test]
fn a_descriptor_stays_on_its_file_when_another_file_is_renamed_over_its_path() {
    let root = root_with_file("renamed");
    std::fs::write(root.join("g.txt"), "GGGGGGGGGGGGGG").unwrap();
    let bin = compile(
        "fs_renamed",
        &hello_with(STREAMS_IMPORTS, STREAMS),
        &preopen_host_with(&root, "with_writable_preopen"),
        "inst.fs_renamed();",
    );
    run_ok(&bin);
    let now = std::fs::read_to_string(root.join("f.txt")).unwrap();
    assert_eq!(now, "GGGGGGGGGGGGGG");
}

/// The imports of the socket tests. A call that returns `result<_, error-code>` puts the
/// tag at 512 and the error's tag byte at 513; one that returns a handle (or a tuple of
/// them) puts the tag at 512, the handles from 516 and an error's tag byte at 516.
/// `start-bind` and `start-connect` take the socket, the network, the address as a
/// flattened `ip-socket-address` (the tag, then 11 words: `port a b c d` for ipv4,
/// `port flow a0..a7 scope` for ipv6) and the return pointer.
/// Address families: ipv4 0, ipv6 1. Error codes: access-denied 1, invalid-argument 3,
/// not-in-progress 7, would-block 8, invalid-state 9, connection-refused 14.
const SOCKET_IMPORTS: &str = r#"
  (import "wasi:sockets/instance-network@0.2.0" "instance-network" (func $network (result i32)))
  (import "wasi:sockets/network@0.2.0" "[resource-drop]network" (func $drop_network (param i32)))
  (import "wasi:sockets/tcp-create-socket@0.2.0" "create-tcp-socket" (func $create (param i32 i32)))
  (import "wasi:sockets/tcp@0.2.0" "[resource-drop]tcp-socket" (func $drop_socket (param i32)))
  (import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.start-bind" (func $start_bind (param i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32)))
  (import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.finish-bind" (func $finish_bind (param i32 i32)))
  (import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.start-listen" (func $start_listen (param i32 i32)))
  (import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.finish-listen" (func $finish_listen (param i32 i32)))
  (import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.start-connect" (func $start_connect (param i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32)))
  (import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.finish-connect" (func $finish_connect (param i32 i32)))
  (import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.accept" (func $accept (param i32 i32)))
  (import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.local-address" (func $local_address (param i32 i32)))
  (import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.remote-address" (func $remote_address (param i32 i32)))
  (import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.subscribe" (func $socket_subscribe (param i32) (result i32)))
  (import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.shutdown" (func $shutdown (param i32 i32 i32)))
  (import "wasi:io/streams@0.2.0" "[method]input-stream.read" (func $read (param i32 i64 i32)))
  (import "wasi:io/streams@0.2.0" "[method]input-stream.subscribe" (func $in_subscribe (param i32) (result i32)))
  (data (i32.const 640) "ping")
  (data (i32.const 650) "pong")
"#;

const SOCKET_HELPERS: &str = r#"
  ;; the `result` at 512 is ok / the error `code`, whose tag byte is `off` bytes in
  (func $ok
    (if (i32.load8_u (i32.const 512)) (then unreachable)))
  (func $err_at (param $off i32) (param $code i32)
    (if (i32.ne (i32.load8_u (i32.const 512)) (i32.const 1)) (then unreachable))
    (if (i32.ne (i32.load8_u (i32.add (i32.const 512) (local.get $off))) (local.get $code)) (then unreachable)))
  (func $err (param $code i32)
    (call $err_at (i32.const 1) (local.get $code)))
  ;; the ipv4 address `a.b.c.d:port` (ipv6 `::` and the like are written out by the tests)
  (func $bind4 (param $s i32) (param $a i32) (param $b i32) (param $c i32) (param $d i32) (param $port i32)
    (i32.store (i32.const 512) (i32.const 0x09090909))
    (call $start_bind (local.get $s) (call $network) (i32.const 0)
      (local.get $port) (local.get $a) (local.get $b) (local.get $c) (local.get $d)
      (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0)
      (i32.const 512)))
  (func $connect4 (param $s i32) (param $a i32) (param $b i32) (param $c i32) (param $d i32) (param $port i32)
    (i32.store (i32.const 512) (i32.const 0x09090909))
    (call $start_connect (local.get $s) (call $network) (i32.const 0)
      (local.get $port) (local.get $a) (local.get $b) (local.get $c) (local.get $d)
      (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0)
      (i32.const 512)))
  ;; a new socket of address family `$family`
  (func $new_socket (param $family i32) (result i32)
    (call $create (local.get $family) (i32.const 512))
    (call $ok)
    (i32.load (i32.const 516)))
  ;; blocks until the pollable is ready, then drops it
  (func $wait (param $p i32)
    (call $block (local.get $p))
    (call $drop_pollable (local.get $p)))
  ;; `read` of the stream: the bytes at 640.. are expected, or the error `closed` if `$want` is 0
  (func $read_exactly (param $stream i32) (param $want i32)
    (call $wait (call $in_subscribe (local.get $stream)))
    (i32.store (i32.const 600) (i32.const 0x09090909))
    (call $read (local.get $stream) (i64.const 16) (i32.const 600))
    (if (local.get $want)
      (then
        (if (i32.load8_u (i32.const 600)) (then unreachable))
        (if (i32.ne (i32.load (i32.const 608)) (i32.const 4)) (then unreachable))
        (if (i32.ne (i32.load (i32.load (i32.const 604))) (i32.load (local.get $want))) (then unreachable)))
      (else
        (if (i32.ne (i32.load8_u (i32.const 600)) (i32.const 1)) (then unreachable))
        (if (i32.ne (i32.load8_u (i32.const 604)) (i32.const 1)) (then unreachable)))))
"#;

/// `net_echo`: one guest is both ends of a connection on loopback. It listens on port 0,
/// connects to the port `local-address` reports, accepts, sends `ping` one way and `pong`
/// back, then shuts the sending side down and sees the end of input.
const NET_ECHO: &str = r#"
  (func (export "net_echo")
    (local $srv i32) (local $cli i32) (local $port i32)
    (local $cin i32) (local $cout i32) (local $conn i32) (local $sin i32) (local $sout i32)
    (local.set $srv (call $new_socket (i32.const 0)))
    (call $bind4 (local.get $srv) (i32.const 127) (i32.const 0) (i32.const 0) (i32.const 1) (i32.const 0))
    (call $ok)
    (call $finish_bind (local.get $srv) (i32.const 512))
    (call $ok)
    (call $start_listen (local.get $srv) (i32.const 512))
    (call $ok)
    (call $finish_listen (local.get $srv) (i32.const 512))
    (call $ok)
    ;; result<ip-socket-address> at 528: ipv4 (tag 0), port at 536, address at 538
    (call $local_address (local.get $srv) (i32.const 528))
    (if (i32.load8_u (i32.const 528)) (then unreachable))
    (if (i32.load8_u (i32.const 532)) (then unreachable))
    (if (i32.ne (i32.load8_u (i32.const 538)) (i32.const 127)) (then unreachable))
    (local.set $port (i32.load16_u (i32.const 536)))
    (if (i32.eqz (local.get $port)) (then unreachable))
    ;; nothing to accept yet
    (call $accept (local.get $srv) (i32.const 512))
    (call $err_at (i32.const 4) (i32.const 8))
    ;; the client
    (local.set $cli (call $new_socket (i32.const 0)))
    (call $connect4 (local.get $cli) (i32.const 127) (i32.const 0) (i32.const 0) (i32.const 1) (local.get $port))
    (call $ok)
    (call $finish_connect (local.get $cli) (i32.const 512))
    (call $ok)
    (local.set $cin (i32.load (i32.const 516)))
    (local.set $cout (i32.load (i32.const 520)))
    ;; the listener is ready, and accepts
    (call $wait (call $socket_subscribe (local.get $srv)))
    (call $accept (local.get $srv) (i32.const 512))
    (call $ok)
    (local.set $conn (i32.load (i32.const 516)))
    (local.set $sin (i32.load (i32.const 520)))
    (local.set $sout (i32.load (i32.const 524)))
    ;; the accepted end sees the client's address as its remote
    (call $remote_address (local.get $conn) (i32.const 528))
    (if (i32.load8_u (i32.const 528)) (then unreachable))
    (if (i32.ne (i32.load8_u (i32.const 538)) (i32.const 127)) (then unreachable))
    ;; ping out, pong back
    (call $write (local.get $cout) (i32.const 640) (i32.const 4) (i32.const 576))
    (if (i32.load8_u (i32.const 576)) (then unreachable))
    (call $read_exactly (local.get $sin) (i32.const 640))
    (call $write (local.get $sout) (i32.const 650) (i32.const 4) (i32.const 576))
    (if (i32.load8_u (i32.const 576)) (then unreachable))
    (call $read_exactly (local.get $cin) (i32.const 650))
    ;; the end of the client's output is the end of the accepted input
    (call $shutdown (local.get $cli) (i32.const 1) (i32.const 512))
    (call $ok)
    (call $read_exactly (local.get $sin) (i32.const 0))
    (call $drop_stream (local.get $cin))
    (call $drop_out (local.get $cout))
    (call $drop_stream (local.get $sin))
    (call $drop_out (local.get $sout))
    (call $drop_socket (local.get $cli))
    (call $drop_socket (local.get $conn))
    (call $drop_socket (local.get $srv))
    (call $drop_network (call $network)))
"#;

#[test]
fn tcp_sockets_connect_accept_and_exchange_bytes_over_loopback() {
    let bin = compile(
        "net_echo",
        &hello_with(SOCKET_IMPORTS, &format!("{SOCKET_HELPERS}{NET_ECHO}")),
        "P2Host::new(vec![], vec![])",
        "inst.net_echo();",
    );
    run_ok(&bin);
}

/// `net_checks`: addresses outside loopback are `access-denied`, and calls in the wrong
/// state or with the wrong address family fail with the error the interface names.
const NET_CHECKS: &str = r#"
  (func (export "net_checks") (local $s i32) (local $v6 i32) (local $l i32)
    (local.set $s (call $new_socket (i32.const 0)))
    ;; not loopback: access-denied (1), for bind and connect
    (call $bind4 (local.get $s) (i32.const 8) (i32.const 8) (i32.const 8) (i32.const 8) (i32.const 0))
    (call $err (i32.const 1))
    (call $bind4 (local.get $s) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0))
    (call $err (i32.const 1))
    (call $connect4 (local.get $s) (i32.const 8) (i32.const 8) (i32.const 8) (i32.const 8) (i32.const 53))
    (call $err (i32.const 1))
    ;; nothing has been started on it
    (call $finish_bind (local.get $s) (i32.const 512))
    (call $err (i32.const 7))
    (call $finish_connect (local.get $s) (i32.const 512))
    (call $err_at (i32.const 4) (i32.const 7))
    (call $finish_listen (local.get $s) (i32.const 512))
    (call $err (i32.const 7))
    ;; not bound or not listening: invalid-state (9)
    (call $start_listen (local.get $s) (i32.const 512))
    (call $err (i32.const 9))
    (call $accept (local.get $s) (i32.const 512))
    (call $err_at (i32.const 4) (i32.const 9))
    (call $local_address (local.get $s) (i32.const 512))
    (call $err_at (i32.const 4) (i32.const 9))
    (call $remote_address (local.get $s) (i32.const 512))
    (call $err_at (i32.const 4) (i32.const 9))
    ;; bound once: bound for good
    (local.set $l (call $new_socket (i32.const 0)))
    (call $bind4 (local.get $l) (i32.const 127) (i32.const 0) (i32.const 0) (i32.const 1) (i32.const 0))
    (call $ok)
    (call $finish_bind (local.get $l) (i32.const 512))
    (call $ok)
    (call $bind4 (local.get $l) (i32.const 127) (i32.const 0) (i32.const 0) (i32.const 1) (i32.const 0))
    (call $err (i32.const 9))
    ;; an ipv4 address on an ipv6 socket (invalid-argument 3), `::` on it is not loopback
    (local.set $v6 (call $new_socket (i32.const 1)))
    (call $bind4 (local.get $v6) (i32.const 127) (i32.const 0) (i32.const 0) (i32.const 1) (i32.const 0))
    (call $err (i32.const 3))
    (i32.store (i32.const 512) (i32.const 0x09090909))
    (call $start_bind (local.get $v6) (call $network) (i32.const 1)
      (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0)
      (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 512))
    (call $err (i32.const 1))
    ;; nobody listens on port 1: the connection is refused (14), reported by `finish-connect`
    (call $connect4 (local.get $s) (i32.const 127) (i32.const 0) (i32.const 0) (i32.const 1) (i32.const 1))
    (call $ok)
    (call $finish_connect (local.get $s) (i32.const 512))
    (call $err_at (i32.const 4) (i32.const 14)))
  ;; with `with_external_network` the unspecified address is bindable
  (func (export "net_external") (local $s i32)
    (local.set $s (call $new_socket (i32.const 0)))
    (call $bind4 (local.get $s) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0))
    (call $ok)
    (call $finish_bind (local.get $s) (i32.const 512))
    (call $ok)
    (call $local_address (local.get $s) (i32.const 528))
    (if (i32.load8_u (i32.const 528)) (then unreachable))
    (if (i32.eqz (i32.load16_u (i32.const 536))) (then unreachable)))
"#;

#[test]
fn tcp_sockets_refuse_addresses_outside_loopback_and_calls_in_the_wrong_state() {
    let bin = compile(
        "net_checks",
        &hello_with(SOCKET_IMPORTS, &format!("{SOCKET_HELPERS}{NET_CHECKS}")),
        "P2Host::new(vec![], vec![])",
        "inst.net_checks();",
    );
    run_ok(&bin);
}

#[test]
fn with_external_network_permits_binding_beyond_loopback() {
    let bin = compile(
        "net_external",
        &hello_with(SOCKET_IMPORTS, &format!("{SOCKET_HELPERS}{NET_CHECKS}")),
        "P2Host::new(vec![], vec![]).with_external_network()",
        "inst.net_external();",
    );
    run_ok(&bin);
}
