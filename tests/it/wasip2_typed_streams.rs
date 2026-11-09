//! The output-stream methods of `wasi:io/streams` on stdout and stderr with the typed host:
//! `write`, `blocking-write-and-flush`, `flush`, `write-zeroes` and `blocking-write-zeroes-and-flush`
//! (files and connections are covered next to their interfaces). The imports are declared
//! in WIT text.

use crate::wasip2_typed_host::{assert_succeeded, component_from_wit, run_program};

const IO: &str = r#"
package wasi:io@0.2.0;
interface error {
  resource error;
}
interface poll {
  resource pollable;
}
interface streams {
  use error.{error};
  use poll.{pollable};
  variant stream-error { last-operation-failed(error), closed }
  resource output-stream {
    flush: func() -> result<_, stream-error>;
    write: func(contents: list<u8>) -> result<_, stream-error>;
    blocking-write-and-flush: func(contents: list<u8>) -> result<_, stream-error>;
    write-zeroes: func(len: u64) -> result<_, stream-error>;
    blocking-write-zeroes-and-flush: func(len: u64) -> result<_, stream-error>;
  }
}
"#;

const CLI: &str = r#"
package wasi:cli@0.2.0;
interface stdout {
  use wasi:io/streams@0.2.0.{output-stream};
  get-stdout: func() -> output-stream;
}
interface stderr {
  use wasi:io/streams@0.2.0.{output-stream};
  get-stderr: func() -> output-stream;
}
"#;

const WORLD: &str = r#"
package test:t;
world w {
  import wasi:io/streams@0.2.0;
  import wasi:cli/stdout@0.2.0;
  import wasi:cli/stderr@0.2.0;
}
"#;

const CORE: &str = r#"
(module
  (import "wasi:io/error@0.2.0" "[resource-drop]error" (func (param i32)))
  (import "wasi:io/poll@0.2.0" "[resource-drop]pollable" (func (param i32)))
  (import "wasi:io/streams@0.2.0" "[resource-drop]output-stream" (func (param i32)))
  (import "wasi:io/streams@0.2.0" "[method]output-stream.flush" (func $flush (param i32 i32)))
  (import "wasi:io/streams@0.2.0" "[method]output-stream.write" (func $write (param i32 i32 i32 i32)))
  (import "wasi:io/streams@0.2.0" "[method]output-stream.blocking-write-and-flush" (func $bwf (param i32 i32 i32 i32)))
  (import "wasi:io/streams@0.2.0" "[method]output-stream.write-zeroes" (func $zeroes (param i32 i64 i32)))
  (import "wasi:io/streams@0.2.0" "[method]output-stream.blocking-write-zeroes-and-flush" (func $bzf (param i32 i64 i32)))
  (import "wasi:cli/stdout@0.2.0" "get-stdout" (func $stdout (result i32)))
  (import "wasi:cli/stderr@0.2.0" "get-stderr" (func $stderr (result i32)))
  (memory (export "memory") 1)
  (global $brk (mut i32) (i32.const 1024))
  (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32) (local $p i32)
    (local.set $p (i32.and (i32.add (global.get $brk) (i32.sub (local.get 2) (i32.const 1)))
                           (i32.sub (i32.const 0) (local.get 2))))
    (global.set $brk (i32.add (local.get $p) (local.get 3)))
    (local.get $p))
  (data (i32.const 64) "hello")
  ;; each returns 0 for ok, 1 for an error (the result tag at 200)
  (func (export "stdout_handle") (result i32) (call $stdout))
  (func (export "stderr_handle") (result i32) (call $stderr))
  (func (export "bwf") (param $s i32) (param $p i32) (param $n i32) (result i32)
    (call $bwf (local.get $s) (local.get $p) (local.get $n) (i32.const 200))
    (i32.load8_u (i32.const 200)))
  (func (export "write") (param $s i32) (param $p i32) (param $n i32) (result i32)
    (call $write (local.get $s) (local.get $p) (local.get $n) (i32.const 200))
    (i32.load8_u (i32.const 200)))
  (func (export "flush") (param $s i32) (result i32)
    (call $flush (local.get $s) (i32.const 200))
    (i32.load8_u (i32.const 200)))
  (func (export "zeroes") (param $s i32) (param $n i64) (result i32)
    (call $zeroes (local.get $s) (local.get $n) (i32.const 200))
    (i32.load8_u (i32.const 200)))
  (func (export "bzf") (param $s i32) (param $n i64) (result i32)
    (call $bzf (local.get $s) (local.get $n) (i32.const 200))
    (i32.load8_u (i32.const 200))))
"#;

fn run(body: &str) -> std::process::Output {
    run_program(
        &component_from_wit(&[IO, CLI], WORLD, "w"),
        CORE,
        "WasiHost::new(Vec::new())",
        body,
    )
}

#[test]
fn blocking_writes_and_write_zeroes_reach_stdout_in_order() {
    let out = run("let o = inst.stdout_handle();
         assert_eq!(inst.bwf(o, 64, 5), 0);
         assert_eq!(inst.zeroes(o, 2), 0);
         assert_eq!(inst.flush(o), 0);
         assert_eq!(inst.bzf(o, 1), 0);
         assert_eq!(inst.bwf(o, 64, 2), 0);");
    assert_succeeded(&out);
    assert_eq!(out.stdout, b"hello\0\0\0he");
}

#[test]
fn blocking_write_and_flush_reaches_stderr() {
    let out = run("let e = inst.stderr_handle();
         assert_eq!(inst.bwf(e, 64, 5), 0);
         assert_eq!(inst.flush(e), 0);");
    assert_succeeded(&out);
    assert!(out.stdout.is_empty());
    assert_eq!(out.stderr, b"hello");
}

#[test]
fn a_write_over_the_check_write_budget_traps_and_writes_nothing() {
    for call in [
        "inst.bzf(o, 4097)",
        "inst.zeroes(o, 4097)",
        "inst.bwf(o, 0, 4097)",
        "inst.write(o, 0, 4097)",
    ] {
        let out = run(&format!("let o = inst.stdout_handle(); {call};"));
        assert!(!out.status.success(), "{call} did not trap");
        assert!(out.stdout.is_empty(), "{call}");
    }
}

#[test]
fn a_write_of_exactly_the_check_write_budget_reaches_stdout() {
    let out = run("let o = inst.stdout_handle();
         assert_eq!(inst.write(o, 64, 5), 0);
         assert_eq!(inst.write(o, 0, 4096), 0);
         assert_eq!(inst.flush(o), 0);");
    assert_succeeded(&out);
    let mut memory = vec![0u8; 4096];
    memory[64..69].copy_from_slice(b"hello"); // the data segment at 64
    let expected = [b"hello".as_slice(), &memory].concat();
    assert_eq!(out.stdout, expected);
}
