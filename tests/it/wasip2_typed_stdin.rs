//! The real process stdin on the typed host: `read` never blocks (an empty list means
//! "nothing yet", `closed` the end of the input) and a stdin pollable wakes on data and at
//! the end of the input. The imports are declared in WIT text.

use std::process::{Child, Command, Stdio};

use crate::wasip2_typed_host::{assert_succeeded, build_program, component_from_wit};

const IO: &str = r#"
package wasi:io@0.2.0;
interface error {
  resource error;
}
interface poll {
  resource pollable {
    block: func();
  }
  poll: func(in: list<borrow<pollable>>) -> list<u32>;
}
interface streams {
  use error.{error};
  use poll.{pollable};
  variant stream-error { last-operation-failed(error), closed }
  resource input-stream {
    read: func(len: u64) -> result<list<u8>, stream-error>;
    subscribe: func() -> pollable;
  }
  resource output-stream {
    write: func(contents: list<u8>) -> result<_, stream-error>;
  }
}
"#;

const CLI: &str = r#"
package wasi:cli@0.2.0;
interface stdin {
  use wasi:io/streams@0.2.0.{input-stream};
  get-stdin: func() -> input-stream;
}
interface stdout {
  use wasi:io/streams@0.2.0.{output-stream};
  get-stdout: func() -> output-stream;
}
"#;

const CLOCKS: &str = r#"
package wasi:clocks@0.2.0;
interface monotonic-clock {
  use wasi:io/poll@0.2.0.{pollable};
  subscribe-duration: func(when: u64) -> pollable;
}
"#;

const WORLD: &str = r#"
package test:t;
world w {
  import wasi:io/streams@0.2.0;
  import wasi:cli/stdin@0.2.0;
  import wasi:cli/stdout@0.2.0;
  import wasi:clocks/monotonic-clock@0.2.0;
}
"#;

const CORE: &str = r#"
(module
  (import "wasi:io/error@0.2.0" "[resource-drop]error" (func (param i32)))
  (import "wasi:io/poll@0.2.0" "[resource-drop]pollable" (func (param i32)))
  (import "wasi:io/poll@0.2.0" "[method]pollable.block" (func (param i32)))
  (import "wasi:io/poll@0.2.0" "poll" (func $poll (param i32 i32 i32)))
  (import "wasi:io/streams@0.2.0" "[resource-drop]input-stream" (func (param i32)))
  (import "wasi:io/streams@0.2.0" "[resource-drop]output-stream" (func (param i32)))
  (import "wasi:io/streams@0.2.0" "[method]input-stream.read" (func $read (param i32 i64 i32)))
  (import "wasi:io/streams@0.2.0" "[method]input-stream.subscribe" (func $sub_in (param i32) (result i32)))
  (import "wasi:io/streams@0.2.0" "[method]output-stream.write" (func $write (param i32 i32 i32 i32)))
  (import "wasi:cli/stdin@0.2.0" "get-stdin" (func $stdin (result i32)))
  (import "wasi:cli/stdout@0.2.0" "get-stdout" (func $stdout (result i32)))
  (import "wasi:clocks/monotonic-clock@0.2.0" "subscribe-duration" (func $dur (param i64) (result i32)))
  (memory (export "memory") 1)
  (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32) (local $p i32)
    (local.set $p (i32.and (i32.add (global.get $brk) (i32.sub (local.get 2) (i32.const 1)))
                           (i32.sub (i32.const 0) (local.get 2))))
    (global.set $brk (i32.add (local.get $p) (local.get 3)))
    (local.get $p))
  (global $brk (mut i32) (i32.const 1024))
  (data (i32.const 64) "hi\n")
  ;; waits until stdin has data or is at its end: a fresh pollable, as `std` does
  (func $wait_stdin
    (i32.store (i32.const 112) (call $sub_in (call $stdin)))
    (call $poll (i32.const 112) (i32.const 1) (i32.const 116)))
  ;; polls stdin and a timer `ns` nanoseconds away: returns the one index that is ready
  (func $poll_stdin_or_after (param $ns i64) (result i32)
    (i32.store (i32.const 96) (call $sub_in (call $stdin)))
    (i32.store (i32.const 100) (call $dur (local.get $ns)))
    (call $poll (i32.const 96) (i32.const 2) (i32.const 104))
    (if (i32.ne (i32.load (i32.const 108)) (i32.const 1)) (then unreachable))
    (i32.load (i32.load (i32.const 104))))
  ;; nothing has been written to stdin (and it is still open): `read` returns an empty
  ;; list at once, and `poll` on it times out with the 30 ms timer
  (func (export "idle_stdin")
    (call $read (call $stdin) (i64.const 4) (i32.const 200))
    (if (i32.load8_u (i32.const 200)) (then unreachable))
    (if (i32.load (i32.const 208)) (then unreachable))
    (if (i32.ne (call $poll_stdin_or_after (i64.const 30000000)) (i32.const 1)) (then unreachable)))
  ;; nothing arrives for 200 ms: a 100 ms timer wins (index 1). Then "x" arrives while
  ;; `poll` waits (the other timer is ten seconds away), and the input ends: `poll`
  ;; returns for the end too, and `read` reports `closed`
  (func (export "wait_stdin_data")
    ;; tell the test (with "hi\n") that the program is running
    (call $write (call $stdout) (i32.const 64) (i32.const 3) (i32.const 32))
    (if (i32.ne (call $poll_stdin_or_after (i64.const 100000000)) (i32.const 1)) (then unreachable))
    (if (call $poll_stdin_or_after (i64.const 10000000000)) (then unreachable))
    (call $read (call $stdin) (i64.const 16) (i32.const 200))
    (if (i32.load8_u (i32.const 200)) (then unreachable))
    (if (i32.ne (i32.load (i32.const 208)) (i32.const 1)) (then unreachable))
    (if (i32.ne (i32.load8_u (i32.load (i32.const 204))) (i32.const 120)) (then unreachable))
    (call $wait_stdin)
    (call $read (call $stdin) (i64.const 16) (i32.const 200))
    (if (i32.ne (i32.load8_u (i32.const 200)) (i32.const 1)) (then unreachable))
    (if (i32.ne (i32.load8_u (i32.const 204)) (i32.const 1)) (then unreachable)))
  ;; stdin holds "ab\ncd": reads it, then end of input is `closed` (error tag 1 at 204)
  (func (export "read_stdin")
    (call $wait_stdin)
    ;; a zero length is an empty list, not the end of input
    (call $read (call $stdin) (i64.const 0) (i32.const 200))
    (if (i32.load8_u (i32.const 200)) (then unreachable))
    (if (i32.load (i32.const 208)) (then unreachable))
    (call $read (call $stdin) (i64.const 4) (i32.const 200))
    (if (i32.load8_u (i32.const 200)) (then unreachable))
    (if (i32.ne (i32.load (i32.const 208)) (i32.const 4)) (then unreachable))
    (if (i32.ne (i32.load (i32.load (i32.const 204))) (i32.const 0x630a6261)) (then unreachable))
    ;; a length of u64::MAX (-1 as an i64) means "as much as there is"
    (call $read (call $stdin) (i64.const -1) (i32.const 200))
    (if (i32.load8_u (i32.const 200)) (then unreachable))
    (if (i32.ne (i32.load (i32.const 208)) (i32.const 1)) (then unreachable))
    (if (i32.ne (i32.load8_u (i32.load (i32.const 204))) (i32.const 100)) (then unreachable))
    (call $wait_stdin)
    (call $read (call $stdin) (i64.const 4) (i32.const 200))
    (if (i32.ne (i32.load8_u (i32.const 200)) (i32.const 1)) (then unreachable))
    (if (i32.ne (i32.load8_u (i32.const 204)) (i32.const 1)) (then unreachable))))
"#;

/// Starts the program that runs `body` with all three standard streams piped.
fn spawn(body: &str) -> Child {
    let bin = build_program(
        &component_from_wit(&[IO, CLI, CLOCKS], WORLD, "w"),
        CORE,
        "TypedHost::new(Vec::new())",
        body,
    );
    Command::new(bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn generated binary")
}

/// Waits up to 30 seconds (the machine may be busy) for `child` to exit successfully;
/// kills it and fails if it does not exit, or fails.
fn finish_within_30_seconds(mut child: Child) {
    let started = std::time::Instant::now();
    let mut blocked = false;
    while child.try_wait().unwrap().is_none() {
        if started.elapsed() > std::time::Duration::from_secs(30) {
            child.kill().unwrap();
            blocked = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(!blocked, "the generated binary blocked");
    assert_succeeded(&child.wait_with_output().unwrap());
}

#[test]
fn a_host_reads_stdin_and_reports_end_of_input_as_closed() {
    use std::io::Write;
    let mut child = spawn("inst.read_stdin();");
    child.stdin.take().unwrap().write_all(b"ab\ncd").unwrap();
    finish_within_30_seconds(child);
}

#[test]
fn reading_an_idle_stdin_returns_an_empty_list_without_blocking() {
    let mut child = spawn("inst.idle_stdin();");
    // the write end stays open, so stdin is idle, not at its end
    let _stdin = child.stdin.take().unwrap();
    finish_within_30_seconds(child);
}

#[test]
fn polling_stdin_wakes_when_data_arrives_and_at_the_end_of_input() {
    use std::io::{Read, Write};
    let mut child = spawn("inst.wait_stdin_data();");
    let mut stdin = child.stdin.take().unwrap();
    // the program is up (its first poll, with a 100 ms timer, follows at once)
    let mut hi = [0u8; 3];
    child.stdout.as_mut().unwrap().read_exact(&mut hi).unwrap();
    // long enough for the 100 ms timer to win even on a loaded machine
    std::thread::sleep(std::time::Duration::from_millis(500));
    stdin.write_all(b"x").unwrap();
    // let the program read "x" before the input ends
    std::thread::sleep(std::time::Duration::from_millis(200));
    drop(stdin);
    // a `poll` that missed the data would wait out its ten-second timer and the guest's
    // checks would fail
    finish_within_30_seconds(child);
}
