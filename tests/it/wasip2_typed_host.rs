//! The typed WASI preview2 host of the `wasm2rs-wasi` crate: it implements the library's
//! `Host` trait (no canonical-ABI code of its own) for the interfaces a `println!` program
//! uses. The component declares those imports; a core module imports
//! the same lowered functions, is transpiled with `named_imports` + `reentrant_imports` +
//! `named_exports`, and runs with the generated glue and the typed host.

use std::io::Read;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use crate::common;
use wasm2rs::{TranspileOptions, WitOptions, transpile_with_options, wit_host_glue_with_options};

/// A component importing what the WIT `world` of `wit` imports, after the packages in
/// `deps` (each a complete WIT file, pushed in order). The world's imports are encoded
/// with `wit-component` as a component around a generated dummy core module.
pub(crate) fn component_from_wit(deps: &[&str], wit: &str, world: &str) -> Vec<u8> {
    use wit_component::{ComponentEncoder, StringEncoding, dummy_module, embed_component_metadata};
    use wit_parser::{ManglingAndAbi, Resolve};
    let mut resolve = Resolve::default();
    for dep in deps {
        resolve.push_str("dep.wit", dep).unwrap();
    }
    let package = resolve.push_str("test.wit", wit).unwrap();
    let world = resolve.select_world(&[package], Some(world)).unwrap();
    let mut module = dummy_module(&resolve, world, ManglingAndAbi::Standard32);
    embed_component_metadata(&mut module, &resolve, world, StringEncoding::UTF8, false).unwrap();
    ComponentEncoder::default()
        .module(&module)
        .unwrap()
        .validate(true)
        .encode()
        .unwrap()
}

/// Builds the program for `component` and the core module `core_wat`: the glue (in the
/// external-types mode, naming the `wasm2rs_wasi` library that is linked), and a `main` that
/// makes the instance (around the `host` expression) and runs `body`.
pub(crate) fn build_program(
    component: &[u8],
    core_wat: &str,
    host: &str,
    body: &str,
) -> std::path::PathBuf {
    let options = WitOptions {
        external_types: Some("wasm2rs_wasi".to_string()),
        ..WitOptions::default()
    };
    let glue = wit_host_glue_with_options(component, &options).unwrap();
    let opts = TranspileOptions {
        named_imports: true,
        reentrant_imports: true,
        named_exports: true,
        ..TranspileOptions::default()
    };
    let generated = transpile_with_options(&wat::parse_str(core_wat).unwrap(), &opts).unwrap();
    let main = format!("fn main() {{ let mut inst = Instance::new(Glue({host})); {body} }}");
    common::build_with_wasi(
        "wasip2_typed_host",
        &format!("use wasm2rs_wasi::WasiHost;\n{generated}\n{glue}\n{main}\n"),
    )
}

/// How long a generated program may run before the test fails instead of hanging.
const RUN_LIMIT: Duration = Duration::from_secs(60);

pub(crate) fn run_program(component: &[u8], core_wat: &str, host: &str, body: &str) -> Output {
    run_program_within(component, core_wat, host, body, RUN_LIMIT)
}

/// Like [`run_program`], but kills the program and panics if it runs longer than `limit`
/// (a guest blocked forever must fail the test, not hang CI).
#[allow(clippy::panic, reason = "a test helper: a timeout must fail the test")]
pub(crate) fn run_program_within(
    component: &[u8],
    core_wat: &str,
    host: &str,
    body: &str,
    limit: Duration,
) -> Output {
    let mut child = Command::new(build_program(component, core_wat, host, body))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut bytes);
            }
            bytes
        })
    };
    let stdout = drain(child.stdout.take().map(|p| Box::new(p) as _));
    let stderr = drain(child.stderr.take().map(|p| Box::new(p) as _));
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if start.elapsed() >= limit {
            child.kill().unwrap();
            child.wait().unwrap();
            let stderr = stderr.join().unwrap();
            panic!(
                "the generated program timed out after {limit:?}; stderr: {}",
                String::from_utf8_lossy(&stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    Output {
        status,
        stdout: stdout.join().unwrap(),
        stderr: stderr.join().unwrap(),
    }
}

/// The imports of a `println!("hello")` program, as the WIT of wasi 0.2 declares them
/// (the subset this host implements).
const COMPONENT: &str = r#"
(component
  (import "wasi:io/error@0.2.0" (instance $error
    (export "error" (type (sub resource)))))
  (alias export $error "error" (type $err))
  (import "wasi:io/poll@0.2.0" (instance $poll
    (export "pollable" (type $p (sub resource)))
    (export "[method]pollable.block" (func (param "self" (borrow $p))))
    (export "poll" (func (param "in" (list (borrow $p))) (result (list u32))))))
  (alias export $poll "pollable" (type $pollable))
  (import "wasi:io/streams@0.2.0" (instance $streams
    (alias outer 1 $err (type $err))
    (alias outer 1 $pollable (type $pollable))
    (export "input-stream" (type (sub resource)))
    (export "output-stream" (type $os (sub resource)))
    (type $se (variant (case "last-operation-failed" (own $err)) (case "closed")))
    (export "stream-error" (type $stream_error (eq $se)))
    (export "[method]output-stream.check-write"
      (func (param "self" (borrow $os)) (result (result u64 (error $stream_error)))))
    (export "[method]output-stream.write"
      (func (param "self" (borrow $os)) (param "contents" (list u8))
        (result (result (error $stream_error)))))
    (export "[method]output-stream.blocking-flush"
      (func (param "self" (borrow $os)) (result (result (error $stream_error)))))
    (export "[method]output-stream.subscribe"
      (func (param "self" (borrow $os)) (result (own $pollable))))))
  (alias export $streams "output-stream" (type $out))
  (import "wasi:cli/stdout@0.2.0" (instance
    (alias outer 1 $out (type $out))
    (export "get-stdout" (func (result (own $out))))))
  (alias export $streams "input-stream" (type $in))
  (import "wasi:cli/stderr@0.2.0" (instance
    (alias outer 1 $out (type $out))
    (export "get-stderr" (func (result (own $out))))))
  (import "wasi:cli/stdin@0.2.0" (instance
    (alias outer 1 $in (type $in))
    (export "get-stdin" (func (result (own $in))))))
  (import "wasi:cli/terminal-input@0.2.0" (instance $ti
    (export "terminal-input" (type (sub resource)))))
  (alias export $ti "terminal-input" (type $tin))
  (import "wasi:cli/terminal-output@0.2.0" (instance $to
    (export "terminal-output" (type (sub resource)))))
  (alias export $to "terminal-output" (type $tout))
  (import "wasi:cli/terminal-stdin@0.2.0" (instance
    (alias outer 1 $tin (type $tin))
    (export "get-terminal-stdin" (func (result (option (own $tin)))))))
  (import "wasi:cli/terminal-stdout@0.2.0" (instance
    (alias outer 1 $tout (type $tout))
    (export "get-terminal-stdout" (func (result (option (own $tout)))))))
  (import "wasi:cli/terminal-stderr@0.2.0" (instance
    (alias outer 1 $tout (type $tout))
    (export "get-terminal-stderr" (func (result (option (own $tout)))))))
  (import "wasi:clocks/monotonic-clock@0.2.0" (instance
    (alias outer 1 $pollable (type $pollable))
    (export "subscribe-duration" (func (param "when" u64) (result (own $pollable))))))
  (import "wasi:cli/environment@0.2.0" (instance
    (export "get-environment" (func (result (list (tuple string string)))))))
  (import "wasi:cli/exit@0.2.0" (instance
    (export "exit" (func (param "status" (result)))))))
"#;

/// A core module with the lowered imports: `run` writes "hi\n" to stdout, checks the
/// environment has one variable, waits on a pollable, drops handles and flushes; `bye` exits with an error status.
const CORE: &str = r#"
(module
  (import "wasi:io/error@0.2.0" "[resource-drop]error" (func (param i32)))
  (import "wasi:io/poll@0.2.0" "[resource-drop]pollable" (func $drop_pollable (param i32)))
  (import "wasi:io/streams@0.2.0" "[resource-drop]output-stream" (func $drop_out (param i32)))
  (import "wasi:io/streams@0.2.0" "[resource-drop]input-stream" (func (param i32)))
  (import "wasi:cli/terminal-input@0.2.0" "[resource-drop]terminal-input" (func (param i32)))
  (import "wasi:cli/terminal-output@0.2.0" "[resource-drop]terminal-output" (func (param i32)))
  (import "wasi:io/poll@0.2.0" "poll" (func $poll (param i32 i32 i32)))
  (import "wasi:cli/stderr@0.2.0" "get-stderr" (func $stderr (result i32)))
  (import "wasi:cli/stdin@0.2.0" "get-stdin" (func $stdin (result i32)))
  (import "wasi:cli/terminal-stdin@0.2.0" "get-terminal-stdin" (func $tin (param i32)))
  (import "wasi:cli/terminal-stdout@0.2.0" "get-terminal-stdout" (func (param i32)))
  (import "wasi:cli/terminal-stderr@0.2.0" "get-terminal-stderr" (func (param i32)))
  (import "wasi:clocks/monotonic-clock@0.2.0" "subscribe-duration" (func $dur (param i64) (result i32)))
  (import "wasi:io/poll@0.2.0" "[method]pollable.block" (func $block (param i32)))
  (import "wasi:io/streams@0.2.0" "[method]output-stream.check-write" (func $check (param i32 i32)))
  (import "wasi:io/streams@0.2.0" "[method]output-stream.write" (func $write (param i32 i32 i32 i32)))
  (import "wasi:io/streams@0.2.0" "[method]output-stream.blocking-flush" (func $flush (param i32 i32)))
  (import "wasi:io/streams@0.2.0" "[method]output-stream.subscribe" (func $sub (param i32) (result i32)))
  (import "wasi:cli/stdout@0.2.0" "get-stdout" (func $stdout (result i32)))
  (import "wasi:cli/environment@0.2.0" "get-environment" (func $env (param i32)))
  (import "wasi:cli/exit@0.2.0" "exit" (func $exit (param i32)))
  (memory (export "memory") 1)
  (global $brk (mut i32) (i32.const 1024))
  (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32) (local $p i32)
    (local.set $p (i32.and (i32.add (global.get $brk) (i32.sub (local.get 2) (i32.const 1)))
                           (i32.sub (i32.const 0) (local.get 2))))
    (global.set $brk (i32.add (local.get $p) (local.get 3)))
    (local.get $p))
  (data (i32.const 200) "hi\n")
  (func (export "run") (result i32) (local $s i32)
    (local.set $s (call $stdout))
    (call $check (local.get $s) (i32.const 96))
    ;; result<u64, _> (8-aligned): ok tag 0, then the budget at 104
    (if (i32.load8_u (i32.const 96)) (then unreachable))
    (if (i64.eqz (i64.load (i32.const 104))) (then unreachable))
    (call $write (local.get $s) (i32.const 200) (i32.const 3) (i32.const 100))
    (if (i32.load8_u (i32.const 100)) (then unreachable))
    (call $block (call $sub (local.get $s)))
    (call $drop_pollable (i32.const 0))
    (call $flush (local.get $s) (i32.const 100))
    (call $env (i32.const 100))
    (call $drop_out (local.get $s))
    ;; the list's length (second word) is the number of variables
    (i32.load (i32.const 104)))
  ;; the stdin and stderr handles, and a terminal query that finds no terminal (option tag 0)
  (func (export "handles") (result i32)
    (call $tin (i32.const 100))
    (if (i32.load8_u (i32.const 100)) (then unreachable))
    (i32.add (i32.mul (call $stdin) (i32.const 10)) (call $stderr)))
  ;; a 40 ms timer: `poll` waits for it and reports index 0 of one; returns the count
  (func (export "wait") (result i32)
    (i32.store (i32.const 300) (call $dur (i64.const 40000000)))
    (call $poll (i32.const 300) (i32.const 1) (i32.const 100))
    ;; list<u32> at 100: pointer, length; its single element is index 0
    (if (i32.load (i32.load (i32.const 100))) (then unreachable))
    (i32.load (i32.const 104)))
  (func (export "bye") (call $exit (i32.const 1))))
"#;

fn run(env: &str, body: &str) -> Output {
    let host = format!("WasiHost::new({env})");
    run_program(&wat::parse_str(COMPONENT).unwrap(), CORE, &host, body)
}

pub(crate) fn assert_succeeded(out: &Output) {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_println_program_runs_on_the_typed_host() {
    let out = run(
        r#"vec![("K".into(), "V".into())]"#,
        "assert_eq!(inst.run(), 1);",
    );
    assert_succeeded(&out);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "hi\n");
}

#[test]
fn exit_with_an_error_status_exits_with_code_one() {
    let out = run("Vec::new()", "inst.bye();");
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn stdin_stderr_and_a_missing_terminal_have_their_own_handles() {
    assert_succeeded(&run("Vec::new()", "assert_eq!(inst.handles(), 2);"));
}

#[test]
fn poll_waits_for_a_clock_deadline() {
    assert_succeeded(&run(
        "Vec::new()",
        "let start = std::time::Instant::now(); assert_eq!(inst.wait(), 1); \
         assert!(start.elapsed() >= std::time::Duration::from_millis(40));",
    ));
}

#[test]
#[should_panic(expected = "timed out")]
fn a_guest_that_blocks_forever_fails_the_run_instead_of_hanging() {
    run_program_within(
        &wat::parse_str(COMPONENT).unwrap(),
        CORE,
        "WasiHost::new(Vec::new())",
        "let _ = &mut inst; loop { std::thread::park(); }",
        std::time::Duration::from_secs(3),
    );
}
