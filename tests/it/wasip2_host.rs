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
  (import "wasi:io/poll@0.2.0" "[method]pollable.block" (func (param i32)))
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
  (import "wasi:clocks/monotonic-clock@0.2.0" "subscribe-duration" (func (param i64) (result i32)))
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
    (i32.store (i32.const 96) (i32.const 3))
    (i32.store (i32.const 100) (i32.const 3))
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
    let wat = HELLO
        .replacen(
            "(memory (export",
            "(import \"wasi:cli/environment@0.2.0\" \"get-arguments\" (func $args (param i32)))\n  (memory (export",
            1,
        )
        .replacen("(func (export \"bye\")", &format!("{ARGS}\n  (func (export \"bye\")"), 1);
    let host = r#"P2Host::new(vec!["prog".to_string(), "x".to_string()], vec![("K".to_string(), "VX".to_string())])"#;
    let bin = compile("with_arguments", &wat, host, "inst.args();");
    let run = Command::new(&bin).output().expect("run generated binary");
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}
