//! `output-stream.splice` and `blocking-splice` on the typed host: a file to a file, a TCP
//! connection to a file, and stdin to stdout. The imports are declared in WIT text and the
//! guest is hand-written wat; each test body runs in the generated program, where `ROOT`
//! stands for the writable preopen directory.
//!
//! The guest's `splice`/`blocking_splice` return the count moved, or `-1 - the stream-error
//! case` (so `-2` is `closed`).

use std::process::{Command, Stdio};

use crate::wasip2_typed_fs::fresh_root;
use crate::wasip2_typed_host::{assert_succeeded, build_program, component_from_wit, run_program};

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
  resource input-stream {
    read: func(len: u64) -> result<list<u8>, stream-error>;
  }
  resource output-stream {
    splice: func(src: borrow<input-stream>, len: u64) -> result<u64, stream-error>;
    blocking-splice: func(src: borrow<input-stream>, len: u64) -> result<u64, stream-error>;
  }
}
"#;

const FILESYSTEM: &str = r#"
package wasi:filesystem@0.2.0;
interface types {
  use wasi:io/streams@0.2.0.{input-stream, output-stream};
  type filesize = u64;
  flags descriptor-flags {
    read, write, file-integrity-sync, data-integrity-sync, requested-write-sync, mutate-directory
  }
  flags path-flags { symlink-follow }
  flags open-flags { create, directory, exclusive, truncate }
  enum error-code {
    access, would-block, already, bad-descriptor, busy, deadlock, quota, exist, file-too-large,
    illegal-byte-sequence, in-progress, interrupted, invalid, io, is-directory, loop,
    too-many-links, message-size, name-too-long, no-device, no-entry, no-lock,
    insufficient-memory, insufficient-space, not-directory, not-empty, not-recoverable,
    unsupported, no-tty, no-such-device, overflow, not-permitted, pipe, read-only,
    invalid-seek, text-file-busy, cross-device
  }
  resource descriptor {
    read-via-stream: func(offset: filesize) -> result<input-stream, error-code>;
    write-via-stream: func(offset: filesize) -> result<output-stream, error-code>;
    open-at: func(path-flags: path-flags, path: string, open-flags: open-flags, %flags: descriptor-flags) -> result<descriptor, error-code>;
  }
}
interface preopens {
  use types.{descriptor};
  get-directories: func() -> list<tuple<descriptor, string>>;
}
"#;

const SOCKETS: &str = r#"
package wasi:sockets@0.2.0;
interface network {
  resource network;
  enum error-code {
    unknown, access-denied, not-supported, invalid-argument, out-of-memory, timeout,
    concurrency-conflict, not-in-progress, would-block, invalid-state, new-socket-limit,
    address-not-bindable, address-in-use, remote-unreachable, connection-refused,
    connection-reset, connection-aborted, datagram-too-large, name-unresolvable,
    temporary-resolver-failure, permanent-resolver-failure
  }
  enum ip-address-family { ipv4, ipv6 }
  type ipv4-address = tuple<u8, u8, u8, u8>;
  type ipv6-address = tuple<u16, u16, u16, u16, u16, u16, u16, u16>;
  record ipv4-socket-address { port: u16, address: ipv4-address }
  record ipv6-socket-address { port: u16, flow-info: u32, address: ipv6-address, scope-id: u32 }
  variant ip-socket-address { ipv4(ipv4-socket-address), ipv6(ipv6-socket-address) }
}
interface instance-network {
  use network.{network};
  instance-network: func() -> network;
}
interface tcp {
  use wasi:io/streams@0.2.0.{input-stream, output-stream};
  use network.{network, error-code, ip-socket-address};
  resource tcp-socket {
    start-bind: func(network: borrow<network>, local-address: ip-socket-address) -> result<_, error-code>;
    finish-bind: func() -> result<_, error-code>;
    start-listen: func() -> result<_, error-code>;
    finish-listen: func() -> result<_, error-code>;
    local-address: func() -> result<ip-socket-address, error-code>;
    accept: func() -> result<tuple<tcp-socket, input-stream, output-stream>, error-code>;
  }
}
interface tcp-create-socket {
  use network.{error-code, ip-address-family};
  use tcp.{tcp-socket};
  create-tcp-socket: func(address-family: ip-address-family) -> result<tcp-socket, error-code>;
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

const WORLD: &str = r#"
package test:t;
world w {
  import wasi:io/streams@0.2.0;
  import wasi:filesystem/preopens@0.2.0;
  import wasi:filesystem/types@0.2.0;
  import wasi:sockets/network@0.2.0;
  import wasi:sockets/instance-network@0.2.0;
  import wasi:sockets/tcp@0.2.0;
  import wasi:sockets/tcp-create-socket@0.2.0;
  import wasi:cli/stdin@0.2.0;
  import wasi:cli/stdout@0.2.0;
}
"#;

const CORE: &str = r#"
(module
  (import "wasi:io/error@0.2.0" "[resource-drop]error" (func (param i32)))
  (import "wasi:io/poll@0.2.0" "[resource-drop]pollable" (func (param i32)))
  (import "wasi:io/streams@0.2.0" "[resource-drop]input-stream" (func (param i32)))
  (import "wasi:io/streams@0.2.0" "[resource-drop]output-stream" (func $drop_out (param i32)))
  (import "wasi:io/streams@0.2.0" "[method]input-stream.read" (func $read (param i32 i64 i32)))
  (import "wasi:io/streams@0.2.0" "[method]output-stream.splice" (func $splice (param i32 i32 i64 i32)))
  (import "wasi:io/streams@0.2.0" "[method]output-stream.blocking-splice" (func $bsplice (param i32 i32 i64 i32)))
  (import "wasi:filesystem/types@0.2.0" "[resource-drop]descriptor" (func (param i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.read-via-stream" (func $rvs (param i32 i64 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.write-via-stream" (func $wvs (param i32 i64 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.open-at" (func $open (param i32 i32 i32 i32 i32 i32 i32)))
  (import "wasi:filesystem/preopens@0.2.0" "get-directories" (func $dirs (param i32)))
  (import "wasi:sockets/network@0.2.0" "[resource-drop]network" (func (param i32)))
  (import "wasi:sockets/instance-network@0.2.0" "instance-network" (func $instance_network (result i32)))
  (import "wasi:sockets/tcp@0.2.0" "[resource-drop]tcp-socket" (func (param i32)))
  (import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.start-bind"
    (func $start_bind (param i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32)))
  (import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.finish-bind" (func $finish_bind (param i32 i32)))
  (import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.start-listen" (func $start_listen (param i32 i32)))
  (import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.finish-listen" (func $finish_listen (param i32 i32)))
  (import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.local-address" (func $local (param i32 i32)))
  (import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.accept" (func $accept (param i32 i32)))
  (import "wasi:sockets/tcp-create-socket@0.2.0" "create-tcp-socket" (func $create (param i32 i32)))
  (import "wasi:cli/stdin@0.2.0" "get-stdin" (func $stdin (result i32)))
  (import "wasi:cli/stdout@0.2.0" "get-stdout" (func $stdout (result i32)))
  (memory (export "memory") 1)
  (global $brk (mut i32) (i32.const 2048))
  (global $last (mut i32) (i32.const 0))
  (global $s_in (mut i32) (i32.const -1))
  (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32) (local $p i32)
    (local.set $p (i32.and (i32.add (global.get $brk) (i32.sub (local.get 2) (i32.const 1)))
                           (i32.sub (i32.const 0) (local.get 2))))
    (global.set $brk (i32.add (local.get $p) (local.get 3)))
    (local.get $p))
  (data (i32.const 400) "src.txt")
  (data (i32.const 416) "dst.txt")

  (func (export "stdin_handle") (result i32) (call $stdin))
  (func (export "stdout_handle") (result i32) (call $stdout))

  ;; open-at from the preopen: 0 (the handle is `fs_last`) or 100 + the error code
  (func (export "fs_open") (param $p i32) (param $n i32) (param $o i32) (param $d i32) (result i32)
    (call $dirs (i32.const 100))
    (call $open (i32.load (i32.load (i32.const 100))) (i32.const 1) (local.get $p) (local.get $n)
                (local.get $o) (local.get $d) (i32.const 200))
    (if (result i32) (i32.load8_u (i32.const 200))
      (then (i32.add (i32.const 100) (i32.load8_u (i32.const 204))))
      (else (global.set $last (i32.load (i32.const 204))) (i32.const 0))))
  (func (export "fs_last") (result i32) (global.get $last))
  ;; read-via-stream / write-via-stream: the stream handle, or -1
  (func (export "fs_rstream") (param $h i32) (param $offset i64) (result i32)
    (call $rvs (local.get $h) (local.get $offset) (i32.const 240))
    (if (result i32) (i32.load8_u (i32.const 240)) (then (i32.const -1)) (else (i32.load (i32.const 244)))))
  (func (export "fs_wstream") (param $h i32) (param $offset i64) (result i32)
    (call $wvs (local.get $h) (local.get $offset) (i32.const 240))
    (if (result i32) (i32.load8_u (i32.const 240)) (then (i32.const -1)) (else (i32.load (i32.const 244)))))

  (func (export "drop_out") (param i32) (call $drop_out (local.get 0)))
  ;; read: the length of the list, or -1 - the stream-error case
  (func (export "read") (param $h i32) (param $len i64) (result i32)
    (call $read (local.get $h) (local.get $len) (i32.const 220))
    (if (result i32) (i32.load8_u (i32.const 220))
      (then (i32.sub (i32.const -1) (i32.load8_u (i32.const 224))))
      (else (i32.load (i32.const 228)))))
  ;; splice and blocking-splice: the count moved, or -1 - the stream-error case
  (func (export "splice") (param $out i32) (param $in i32) (param $len i64) (result i64)
    (call $splice (local.get $out) (local.get $in) (local.get $len) (i32.const 280))
    (if (result i64) (i32.load8_u (i32.const 280))
      (then (i64.sub (i64.const -1) (i64.load8_u (i32.const 288))))
      (else (i64.load (i32.const 288)))))
  (func (export "blocking_splice") (param $out i32) (param $in i32) (param $len i64) (result i64)
    (call $bsplice (local.get $out) (local.get $in) (local.get $len) (i32.const 280))
    (if (result i64) (i32.load8_u (i32.const 280))
      (then (i64.sub (i64.const -1) (i64.load8_u (i32.const 288))))
      (else (i64.load (i32.const 288)))))

  ;; a status: 0, or 100 + the error byte at `at + 1`
  (func $status (param $at i32) (result i32)
    (if (result i32) (i32.load8_u (local.get $at))
      (then (i32.add (i32.const 100) (i32.load8_u (i32.add (local.get $at) (i32.const 1)))))
      (else (i32.const 0))))
  ;; listen on 127.0.0.1 with a free port: the socket, or -1
  (func (export "listen") (result i32) (local $s i32) (local $net i32)
    (local.set $net (call $instance_network))
    (call $create (i32.const 0) (i32.const 100))
    (if (i32.load8_u (i32.const 100)) (then (return (i32.const -1))))
    (local.set $s (i32.load (i32.const 104)))
    (call $start_bind (local.get $s) (local.get $net) (i32.const 0)
      (i32.const 0) (i32.const 127) (i32.const 0) (i32.const 0) (i32.const 1)
      (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0)
      (i32.const 120))
    (if (call $status (i32.const 120)) (then (return (i32.const -1))))
    (call $finish_bind (local.get $s) (i32.const 120))
    (if (call $status (i32.const 120)) (then (return (i32.const -1))))
    (call $start_listen (local.get $s) (i32.const 120))
    (if (call $status (i32.const 120)) (then (return (i32.const -1))))
    (call $finish_listen (local.get $s) (i32.const 120))
    (if (call $status (i32.const 120)) (then (return (i32.const -1))))
    (local.get $s))
  (func (export "port") (param $s i32) (result i32)
    (call $local (local.get $s) (i32.const 200))
    (i32.load16_u (i32.const 208)))
  ;; accept: 0 (the input stream is `s_in`), or 100 + the error code (108: nothing yet)
  (func (export "accept") (param $s i32) (result i32)
    (call $accept (local.get $s) (i32.const 160))
    (if (i32.eqz (i32.load8_u (i32.const 160)))
      (then (global.set $s_in (i32.load (i32.const 168)))))
    (if (result i32) (i32.load8_u (i32.const 160))
      (then (i32.add (i32.const 100) (i32.load8_u (i32.const 164))))
      (else (i32.const 0))))
  (func (export "s_in") (result i32) (global.get $s_in)))
"#;

/// Guest-side helpers for the test bodies.
const HELPERS: &str = r#"
type Guest = Instance<Glue<WasiHost>>;
/// Opens `src.txt` for reading and `dst.txt` (created, truncated) for writing; the two streams.
#[allow(dead_code)]
fn open_pair(inst: &mut Guest) -> (i32, i32) {
    assert_eq!(inst.fs_open(400, 7, 0, 1), 0);
    let src = inst.fs_last();
    let input = inst.fs_rstream(src, 0);
    assert!(input >= 0);
    assert_eq!(inst.fs_open(416, 7, 9, 2), 0);
    let dst = inst.fs_last();
    let output = inst.fs_wstream(dst, 0);
    assert!(output >= 0);
    (input, output)
}
/// A listening guest socket, a connected host client, and the accepted input stream.
#[allow(dead_code)]
fn connect(inst: &mut Guest) -> (std::net::TcpStream, i32) {
    let s = inst.listen();
    assert!(s >= 0);
    let client = std::net::TcpStream::connect(("127.0.0.1", inst.port(s) as u16)).unwrap();
    for _ in 0..2000 {
        match inst.accept(s) {
            0 => return (client, inst.s_in()),
            108 => std::thread::sleep(std::time::Duration::from_millis(1)),
            other => panic!("accept: {other}"),
        }
    }
    panic!("accept: nothing arrived");
}
/// Calls `splice` until `want` bytes have moved (a connection may deliver in pieces).
#[allow(dead_code)]
fn splice_until(inst: &mut Guest, out: i32, input: i32, want: i64) {
    let mut moved = 0;
    for _ in 0..2000 {
        let n = inst.splice(out, input, 4096);
        assert!(n >= 0, "splice: {n}");
        moved += n;
        if moved >= want {
            assert_eq!(moved, want);
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    panic!("splice: only {moved} of {want}");
}
"#;

fn run(label: &str, src: &[u8], body: &str) -> std::process::Output {
    let root = fresh_root(label);
    std::fs::write(root.join("src.txt"), src).unwrap();
    let body = body.replace("ROOT", &format!("{:?}", root.to_str().unwrap()));
    run_program(
        &component_from_wit(&[IO, FILESYSTEM, SOCKETS, CLI], WORLD, "w"),
        CORE,
        &format!(
            "WasiHost::new(Vec::new()).with_writable_preopen(std::path::PathBuf::from({:?}))",
            root.to_str().unwrap()
        ),
        &format!("{HELPERS}\n{body}"),
    )
}

fn pattern(n: usize) -> Vec<u8> {
    (0..n).map(|i| u8::try_from(i % 251).unwrap()).collect()
}

#[test]
fn a_file_is_spliced_to_a_file_at_most_4096_bytes_at_a_time() {
    let src = pattern(5000);
    let out = run(
        "file_file",
        &src,
        r#"let (input, output) = open_pair(&mut inst);
         // 10000 asked, 4096 moved; then the rest; then the end
         assert_eq!(inst.splice(output, input, 10000), 4096);
         assert_eq!(inst.splice(output, input, 10000), 904);
         assert_eq!(inst.splice(output, input, 10000), -2);
         assert_eq!(std::fs::read(std::path::Path::new(ROOT).join("dst.txt")).unwrap(), std::fs::read(std::path::Path::new(ROOT).join("src.txt")).unwrap());"#,
    );
    assert_succeeded(&out);
}

#[test]
fn a_shorter_len_moves_exactly_len_bytes_and_the_next_call_continues() {
    let out = run(
        "file_len",
        b"abcdefghij",
        r#"let (input, output) = open_pair(&mut inst);
         assert_eq!(inst.splice(output, input, 3), 3);
         assert_eq!(inst.splice(output, input, 4), 4);
         assert_eq!(inst.splice(output, input, 100), 3);
         assert_eq!(std::fs::read(std::path::Path::new(ROOT).join("dst.txt")).unwrap(), b"abcdefghij");"#,
    );
    assert_succeeded(&out);
}

#[test]
fn a_file_over_4096_bytes_is_moved_in_several_calls_with_equal_content() {
    let out = run(
        "file_big",
        &pattern(20000),
        r#"let (input, output) = open_pair(&mut inst);
         let mut calls = 0;
         let mut moved = 0;
         loop {
             let n = inst.splice(output, input, u64::MAX as i64);
             if n == -2 { break; }
             assert!(n > 0 && n <= 4096, "{n}");
             moved += n;
             calls += 1;
         }
         assert_eq!(moved, 20000);
         assert!(calls >= 5, "{calls}");
         assert_eq!(std::fs::read(std::path::Path::new(ROOT).join("dst.txt")).unwrap(), std::fs::read(std::path::Path::new(ROOT).join("src.txt")).unwrap());"#,
    );
    assert_succeeded(&out);
}

#[test]
fn splicing_from_an_empty_file_reports_the_source_as_closed() {
    let out = run(
        "file_empty",
        b"",
        r#"let (input, output) = open_pair(&mut inst);
         assert_eq!(inst.splice(output, input, 10), -2);
         assert_eq!(inst.blocking_splice(output, input, 10), -2);"#,
    );
    assert_succeeded(&out);
}

#[test]
fn a_zero_len_splice_is_ok_while_the_source_is_open_and_closed_at_its_end() {
    let out = run(
        "file_zero",
        b"xy",
        r#"let (input, output) = open_pair(&mut inst);
         assert_eq!(inst.splice(output, input, 0), 0);
         assert_eq!(inst.blocking_splice(output, input, 0), 0);
         // nothing was consumed
         assert_eq!(inst.splice(output, input, 10), 2);
         assert_eq!(inst.splice(output, input, 0), -2);
         assert_eq!(inst.blocking_splice(output, input, 0), -2);"#,
    );
    assert_succeeded(&out);
}

#[test]
fn splicing_into_a_dropped_output_is_closed_and_consumes_nothing_from_the_source() {
    let out = run(
        "dropped_out",
        b"abcdef",
        r#"let (input, output) = open_pair(&mut inst);
         inst.drop_out(output);
         assert_eq!(inst.splice(output, input, 4), -2);
         assert_eq!(inst.blocking_splice(output, input, 4), -2);
         // a zero length is checked against the output too
         assert_eq!(inst.splice(output, input, 0), -2);
         // the source did not advance
         assert_eq!(inst.read(input, 100), 6);"#,
    );
    assert_succeeded(&out);
}

#[test]
fn a_connection_is_spliced_to_a_file_and_an_idle_one_moves_nothing() {
    let out = run(
        "tcp_file",
        b"",
        r#"use std::io::Write;
         assert_eq!(inst.fs_open(416, 7, 9, 2), 0);
         let dst = inst.fs_last();
         let output = inst.fs_wstream(dst, 0);
         let (mut client, input) = connect(&mut inst);
         // nothing sent yet: ok 0, also for a zero length
         assert_eq!(inst.splice(output, input, 4096), 0);
         assert_eq!(inst.splice(output, input, 0), 0);
         client.write_all(b"hello over tcp").unwrap();
         splice_until(&mut inst, output, input, 14);
         assert_eq!(inst.splice(output, input, 4096), 0);
         assert_eq!(std::fs::read(std::path::Path::new(ROOT).join("dst.txt")).unwrap(), b"hello over tcp");
         // the peer closes: the source is at its end
         drop(client);
         let mut last = 0;
         for _ in 0..2000 {
             last = inst.splice(output, input, 4096);
             if last != 0 { break; }
             std::thread::sleep(std::time::Duration::from_millis(1));
         }
         assert_eq!(last, -2);"#,
    );
    assert_succeeded(&out);
}

#[test]
fn blocking_splice_waits_for_a_connection_to_send_then_moves_the_bytes() {
    let out = run(
        "tcp_blocking",
        b"",
        r#"use std::io::Write;
         assert_eq!(inst.fs_open(416, 7, 9, 2), 0);
         let dst = inst.fs_last();
         let output = inst.fs_wstream(dst, 0);
         let (mut client, input) = connect(&mut inst);
         let sender = std::thread::spawn(move || {
             std::thread::sleep(std::time::Duration::from_millis(300));
             client.write_all(b"late").unwrap();
             client
         });
         let started = std::time::Instant::now();
         assert_eq!(inst.blocking_splice(output, input, 4096), 4);
         assert!(started.elapsed() >= std::time::Duration::from_millis(50), "{:?}", started.elapsed());
         assert_eq!(std::fs::read(std::path::Path::new(ROOT).join("dst.txt")).unwrap(), b"late");
         // the peer closes: the wait ends with `closed`
         drop(sender.join().unwrap());
         assert_eq!(inst.blocking_splice(output, input, 4096), -2);"#,
    );
    assert_succeeded(&out);
}

#[test]
fn blocking_splice_moves_stdin_to_stdout_once_data_arrives_and_ends_with_closed() {
    use std::io::Write;
    let root = fresh_root("stdin_stdout");
    let bin = build_program(
        &component_from_wit(&[IO, FILESYSTEM, SOCKETS, CLI], WORLD, "w"),
        CORE,
        &format!(
            "WasiHost::new(Vec::new()).with_writable_preopen(std::path::PathBuf::from({:?}))",
            root.to_str().unwrap()
        ),
        &r#"let (i, o) = (inst.stdin_handle(), inst.stdout_handle());
         // tell the test that the program is up
         std::fs::write(std::path::Path::new(ROOT).join("up"), "").unwrap();
         // nothing yet: the non-blocking splice moves nothing
         assert_eq!(inst.splice(o, i, 16), 0);
         let started = std::time::Instant::now();
         assert_eq!(inst.blocking_splice(o, i, 16), 3);
         assert!(started.elapsed() >= std::time::Duration::from_millis(50), "{:?}", started.elapsed());
         assert_eq!(inst.blocking_splice(o, i, 16), -2);
         assert_eq!(inst.splice(o, i, 16), -2);"#
            .replace("ROOT", &format!("{:?}", root.to_str().unwrap())),
    );
    let mut child = Command::new(bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    while !root.join("up").exists() {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    std::thread::sleep(std::time::Duration::from_millis(400));
    stdin.write_all(b"xyz").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(100));
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    assert_succeeded(&out);
    assert_eq!(out.stdout, b"xyz");
}

/// A component that imports `read` and `splice` but no pollable and no blocking method:
/// the stdin reader the non-blocking read needs must be kept for it all the same.
#[test]
fn splice_and_read_alone_build_without_the_blocking_methods() {
    use std::io::Write;
    const IO_MIN: &str = r#"
package wasi:io@0.2.0;
interface error {
  resource error;
}
interface streams {
  use error.{error};
  variant stream-error { last-operation-failed(error), closed }
  resource input-stream {
    read: func(len: u64) -> result<list<u8>, stream-error>;
  }
  resource output-stream {
    splice: func(src: borrow<input-stream>, len: u64) -> result<u64, stream-error>;
  }
}
"#;
    const WORLD_MIN: &str = r#"
package test:t;
world w {
  import wasi:io/streams@0.2.0;
  import wasi:cli/stdin@0.2.0;
  import wasi:cli/stdout@0.2.0;
}
"#;
    const CORE_MIN: &str = r#"
(module
  (import "wasi:io/error@0.2.0" "[resource-drop]error" (func (param i32)))
  (import "wasi:io/streams@0.2.0" "[resource-drop]input-stream" (func (param i32)))
  (import "wasi:io/streams@0.2.0" "[resource-drop]output-stream" (func (param i32)))
  (import "wasi:io/streams@0.2.0" "[method]input-stream.read" (func (param i32 i64 i32)))
  (import "wasi:io/streams@0.2.0" "[method]output-stream.splice" (func $splice (param i32 i32 i64 i32)))
  (import "wasi:cli/stdin@0.2.0" "get-stdin" (func $stdin (result i32)))
  (import "wasi:cli/stdout@0.2.0" "get-stdout" (func $stdout (result i32)))
  (memory (export "memory") 1)
  (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32) (i32.const 1024))
  ;; the count moved, or -1 - the stream-error case
  (func (export "move_stdin") (param $len i64) (result i64)
    (call $splice (call $stdout) (call $stdin) (local.get $len) (i32.const 280))
    (if (result i64) (i32.load8_u (i32.const 280))
      (then (i64.sub (i64.const -1) (i64.load8_u (i32.const 288))))
      (else (i64.load (i32.const 288))))))
"#;
    let bin = build_program(
        &component_from_wit(&[IO_MIN, CLI], WORLD_MIN, "w"),
        CORE_MIN,
        "WasiHost::new(Vec::new())",
        "let mut moved = 0;
         loop {
             let n = inst.move_stdin(4096);
             if n == -2 { break; }
             assert!(n >= 0, \"{n}\");
             moved += n;
             std::thread::sleep(std::time::Duration::from_millis(1));
         }
         assert_eq!(moved, 5);",
    );
    let mut child = Command::new(bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"hello").unwrap();
    let out = child.wait_with_output().unwrap();
    assert_succeeded(&out);
    assert_eq!(out.stdout, b"hello");
}
