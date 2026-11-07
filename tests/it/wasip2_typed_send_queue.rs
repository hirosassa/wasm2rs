//! The send queue of a TCP output stream on the typed host: a connection whose peer does
//! not read reports back-pressure through `check-write` (0 once the host's queue is full)
//! and `subscribe` (not ready until room), instead of `write` hanging. The guest is the
//! one of the socket tests (`wasip2_typed_sockets`); the peer is a `std` client in the same
//! program. Every body starts a watchdog, so a hang fails the test instead of stalling it.

use crate::wasip2_typed_host::{assert_succeeded, component_from_wit, run_program};
use crate::wasip2_typed_sockets::{CORE, HELPERS, IO, SOCKETS, WORLD};

/// Guest-side helpers for the bodies: a watchdog; `chunks(n)`, `n` times the 15 bytes at 400
/// (`ping`, six zero bytes, `pong!`); `fill`, which writes those 15 bytes while `check-write`
/// permits, then `ping` once more if it permits that, and returns how many chunks were written
/// and whether the final `ping` was; and `top_off`, which writes single bytes up to 0.
pub(crate) const QUEUE_HELPERS: &str = r#"
#[allow(dead_code)]
fn watchdog() {
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(25));
        eprintln!("watchdog: the program hung");
        std::process::exit(99);
    });
}
#[allow(dead_code)]
fn chunks(n: usize) -> Vec<u8> {
    let mut one = b"ping".to_vec();
    one.extend([0u8; 6]);
    one.extend(b"pong!");
    one.repeat(n)
}
#[allow(dead_code)]
fn top_off(inst: &mut Instance<Glue<TypedHost>>, out: i32) {
    while inst.check_write(out) > 0 {
        assert_eq!(inst.write(out, 400, 1), 0);
    }
}
#[allow(dead_code)]
fn fill(inst: &mut Instance<Glue<TypedHost>>, out: i32) -> (usize, bool) {
    let mut n = 0;
    for _ in 0..2_000_000 {
        let permit = inst.check_write(out);
        assert!(permit >= 0, "check-write: {permit}");
        if permit < 15 {
            let tail = permit >= 4;
            if tail {
                assert_eq!(inst.write(out, 400, 4), 0);
            }
            return (n, tail);
        }
        assert_eq!(inst.write(out, 400, 15), 0);
        n += 1;
    }
    panic!("check-write never reached 0 after {n} writes");
}
"#;

fn run(body: &str) -> std::process::Output {
    run_program(
        &component_from_wit(&[IO, SOCKETS], WORLD, "w"),
        CORE,
        "TypedHost::new(Vec::new())",
        &format!("{HELPERS}\n{QUEUE_HELPERS}\nwatchdog();\n{body}"),
    )
}

const SERVER: &str = "let (s, port) = serve(&mut inst);
         let client = std::net::TcpStream::connect((\"127.0.0.1\", port)).unwrap();
         accept(&mut inst, s);
         let out = inst.s_out();";

#[test]
fn a_peer_that_does_not_read_fills_the_queue_and_check_write_reaches_zero() {
    assert_succeeded(&run(&format!(
        "{SERVER}
         let started = std::time::Instant::now();
         let (n, tail) = fill(&mut inst, out);
         // no write waited for the peer
         assert!(started.elapsed() < std::time::Duration::from_secs(15), \"{{:?}}\", started.elapsed());
         assert!(n > 4096, \"{{n}}\"); // more than the 64 KiB queue: the kernel took some
         assert!(tail || inst.check_write(out) < 4);
         let permit = inst.check_write(out);
         assert!((0..15).contains(&permit), \"{{permit}}\");
         drop(client);"
    )));
}

#[test]
fn a_full_queue_is_not_writable_until_the_peer_reads() {
    assert_succeeded(&run(&format!(
        "{SERVER}
         use std::io::{{Read, Write}};
         let mut client = client;
         // the peer sends a byte, so the guest's input stream is ready (the output stream is not)
         client.write_all(b\"x\").unwrap();
         fill(&mut inst, out);
         // fill leaves less than 15 bytes of permit: write up to 0
         top_off(&mut inst, out);
         let s_in = inst.s_in();
         let (sub_out, sub_in) = (inst.sub_out(out), inst.sub_in(s_in));
         // bit 1 (the input) only
         assert_eq!(inst.poll2(sub_out, sub_in), 0b10);
         assert_eq!(inst.check_write(out), 0);
         // the peer starts reading and keeps reading until told to stop: how much one read
         // frees, and when the kernel sends a window update, depends on the OS
         let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
         let stopped = stop.clone();
         let reader = std::thread::spawn(move || {{
             let mut buf = vec![0u8; 65536];
             client.set_read_timeout(Some(std::time::Duration::from_millis(50))).unwrap();
             while !stopped.load(std::sync::atomic::Ordering::SeqCst) {{
                 let _ = client.read(&mut buf);
             }}
             client
         }});
         inst.block(sub_out);
         assert!(inst.check_write(out) > 0);
         assert_eq!(inst.poll2(sub_out, sub_in) & 1, 1);
         stop.store(true, std::sync::atomic::Ordering::SeqCst);
         drop(reader.join().unwrap());"
    )));
}

#[test]
fn a_write_longer_than_the_permit_of_a_full_queue_traps() {
    let out = run(&format!(
        "{SERVER}
         fill(&mut inst, out);
         top_off(&mut inst, out);
         inst.write(out, 400, 1);
         drop(client);"
    ));
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("more than check-write allows"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The queued bytes a peer that starts reading late receives, in order, once the stream is
/// dropped / the connection is shut down / a blocking write follows: `how` is run in the
/// guest after the queue is full and returns the bytes it adds after the queued ones.
fn queued_bytes_arrive_in_order(how: &str, tail: &str, to_end: bool) {
    assert_succeeded(&run(&format!(
        "{SERVER}
         use std::io::Read;
         let mut client = client;
         let (n, tail) = fill(&mut inst, out);
         let mut expected = chunks(n);
         if tail {{
             expected.extend(b\"ping\");
         }}
         expected.extend({tail});
         let reader = std::thread::spawn(move || {{
             std::thread::sleep(std::time::Duration::from_millis(300));
             client.set_read_timeout(Some(std::time::Duration::from_secs(10))).unwrap();
             let mut got = vec![0u8; expected.len()];
             if {to_end} {{
                 // the end of the stream follows the data
                 got.clear();
                 client.read_to_end(&mut got).unwrap();
             }} else {{
                 client.read_exact(&mut got).unwrap();
             }}
             assert!(got == expected, \"the bytes differ\");
         }});
         let started = std::time::Instant::now();
         {how}
         // the flush waited for the peer, which started reading 300 ms after the fill
         assert!(started.elapsed() >= std::time::Duration::from_millis(150), \"{{:?}}\", started.elapsed());
         reader.join().unwrap();"
    )));
}

#[test]
fn dropping_the_output_stream_flushes_the_queue_in_order() {
    queued_bytes_arrive_in_order("inst.drop_out(out);", "b\"\"", false);
}

#[test]
fn shutting_down_the_send_side_flushes_the_queue_in_order() {
    queued_bytes_arrive_in_order(
        "let sock = inst.s_sock(); assert_eq!(inst.shutdown(sock, 1), 0);",
        "b\"\"",
        true,
    );
}

#[test]
fn shutdown_does_not_flush_the_queue_of_another_connection_that_reused_the_slot() {
    assert_succeeded(&run(
        "let (s, port) = serve(&mut inst);
         let client_a = std::net::TcpStream::connect((\"127.0.0.1\", port)).unwrap();
         accept(&mut inst, s);
         let (sock_a, out_a) = (inst.s_sock(), inst.s_out());
         // A's output stream is dropped; B's takes over its slot
         inst.drop_out(out_a);
         let client_b = std::net::TcpStream::connect((\"127.0.0.1\", port)).unwrap();
         accept(&mut inst, s);
         let out_b = inst.s_out();
         assert_eq!(out_b, out_a, \"the slot was not reused\");
         // B's peer never reads: its queue holds bytes that cannot be sent
         fill(&mut inst, out_b);
         // A's shutdown must not wait for B's peer
         let started = std::time::Instant::now();
         assert_eq!(inst.shutdown(sock_a, 1), 0);
         assert!(started.elapsed() < std::time::Duration::from_secs(5), \"{:?}\", started.elapsed());
         drop((client_a, client_b));",
    ));
}

#[test]
fn a_blocking_write_waits_for_the_queue_and_keeps_the_order() {
    queued_bytes_arrive_in_order(
        "assert_eq!(inst.blocking_write_and_flush(out, 410, 5), 0);",
        "b\"pong!\"",
        false,
    );
}

/// A component that imports only `check-write` and `write` of the output stream: the send
/// queue's helpers it needs must be kept and the rest trimmed without a compile error.
#[test]
fn check_write_and_write_alone_build_without_the_other_stream_methods() {
    const IO_MIN: &str = r#"
package wasi:io@0.2.0;
interface error {
  resource error;
}
interface streams {
  use error.{error};
  variant stream-error { last-operation-failed(error), closed }
  resource output-stream {
    check-write: func() -> result<u64, stream-error>;
    write: func(contents: list<u8>) -> result<_, stream-error>;
  }
}
"#;
    const CLI_MIN: &str = r#"
package wasi:cli@0.2.0;
interface stdout {
  use wasi:io/streams@0.2.0.{output-stream};
  get-stdout: func() -> output-stream;
}
"#;
    const WORLD_MIN: &str = r#"
package test:t;
world w {
  import wasi:io/streams@0.2.0;
  import wasi:cli/stdout@0.2.0;
}
"#;
    const CORE_MIN: &str = r#"
(module
  (import "wasi:io/error@0.2.0" "[resource-drop]error" (func (param i32)))
  (import "wasi:io/streams@0.2.0" "[resource-drop]output-stream" (func (param i32)))
  (import "wasi:io/streams@0.2.0" "[method]output-stream.check-write" (func $check (param i32 i32)))
  (import "wasi:io/streams@0.2.0" "[method]output-stream.write" (func $write (param i32 i32 i32 i32)))
  (import "wasi:cli/stdout@0.2.0" "get-stdout" (func $stdout (result i32)))
  (memory (export "memory") 1)
  (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32) (i32.const 1024))
  (data (i32.const 400) "ok\n")
  (func (export "stdout_handle") (result i32) (call $stdout))
  (func (export "check_write") (param $h i32) (result i64)
    (call $check (local.get $h) (i32.const 184))
    (if (result i64) (i32.load8_u (i32.const 184)) (then (i64.const -1)) (else (i64.load (i32.const 192)))))
  (func (export "write") (param $h i32) (result i32)
    (call $write (local.get $h) (i32.const 400) (i32.const 3) (i32.const 180))
    (i32.load8_u (i32.const 180))))
"#;
    let out = run_program(
        &component_from_wit(&[IO_MIN, CLI_MIN], WORLD_MIN, "w"),
        CORE_MIN,
        "TypedHost::new(Vec::new())",
        "let out = inst.stdout_handle();
         // stdout is not a connection: it keeps permitting 4096 bytes
         assert_eq!(inst.check_write(out), 4096);
         assert_eq!(inst.write(out), 0);
         assert_eq!(inst.check_write(out), 4096);",
    );
    assert_succeeded(&out);
    assert_eq!(out.stdout, b"ok\n");
}

/// The imports of `wasi:io/streams` `flush` and `wasi:cli/exit` added to the socket guest.
const EXIT: &str = r#"
package wasi:cli@0.2.0;
interface exit {
  exit: func(status: result);
}
"#;

fn run_ext(body: &str) -> std::process::Output {
    run_program(
        &ext_component(),
        &ext_core(),
        "TypedHost::new(Vec::new())",
        &format!("{HELPERS}\n{QUEUE_HELPERS}\nwatchdog();\n{body}"),
    )
}

fn ext_component() -> Vec<u8> {
    let io = IO.replace(
        "blocking-flush: func() -> result<_, stream-error>;",
        "blocking-flush: func() -> result<_, stream-error>;\n    flush: func() -> result<_, stream-error>;",
    );
    let world = WORLD.replace(
        "import wasi:io/streams@0.2.0;",
        "import wasi:io/streams@0.2.0;\n  import wasi:cli/exit@0.2.0;",
    );
    component_from_wit(&[&io, SOCKETS, EXIT], &world, "w")
}

/// `CORE` with `flush` (as `nb_flush`) and `exit` (as `bye`) exported.
fn ext_core() -> String {
    CORE.replace(
        "(memory (export \"memory\") 1)",
        "(import \"wasi:io/streams@0.2.0\" \"[method]output-stream.flush\" (func $nbf (param i32 i32)))
  (import \"wasi:cli/exit@0.2.0\" \"exit\" (func $exit (param i32)))
  (memory (export \"memory\") 1)
  (func (export \"nb_flush\") (param $h i32) (result i32)
    (call $nbf (local.get $h) (i32.const 180)) (call $status4 (i32.const 180)))
  (func (export \"bye\") (param i32) (call $exit (local.get 0)))",
    )
}

#[test]
fn a_flush_in_progress_keeps_check_write_at_zero_until_the_queue_is_empty() {
    assert_succeeded(&run_ext(&format!(
        "{SERVER}
         use std::io::Read;
         let mut client = client;
         fill(&mut inst, out);
         top_off(&mut inst, out);
         // a non-blocking flush with bytes still queued: check-write stays 0 until they are sent
         assert_eq!(inst.nb_flush(out), 0);
         assert_eq!(inst.check_write(out), 0);
         let sub_out = inst.sub_out(out);
         let reader = std::thread::spawn(move || {{
             std::thread::sleep(std::time::Duration::from_millis(200));
             client.set_read_timeout(Some(std::time::Duration::from_secs(10))).unwrap();
             let mut buf = vec![0u8; 1 << 16];
             while client.read(&mut buf).unwrap() > 0 {{}}
         }});
         inst.block(sub_out);
         // the queue is empty: the whole 64 KiB is free again (the peer keeps reading)
         let mut room = 0;
         while inst.check_write(out) > 0 && room < 60_000 {{
             assert_eq!(inst.write(out, 400, 1), 0);
             room += 1;
         }}
         assert_eq!(room, 60_000);
         let sock = inst.s_sock();
         assert_eq!(inst.shutdown(sock, 1), 0);
         reader.join().unwrap();"
    )));
}

#[test]
fn a_failed_peer_closes_the_stream_instead_of_trapping_or_hanging() {
    assert_succeeded(&run(&format!(
        "{SERVER}
         fill(&mut inst, out);
         top_off(&mut inst, out);
         let sub_out = inst.sub_out(out);
         // closing a socket with unread data resets the connection
         drop(client);
         inst.block(sub_out);
         assert_eq!(inst.check_write(out), -1);
         assert_eq!(inst.write(out, 400, 4), 101);
         let started = std::time::Instant::now();
         inst.drop_out(out);
         assert!(started.elapsed() < std::time::Duration::from_secs(2), \"{{:?}}\", started.elapsed());"
    )));
}

#[test]
fn exit_flushes_the_queued_bytes_of_a_connection_the_guest_never_dropped() {
    use std::io::{BufRead, Read};
    let body = format!(
        "{HELPERS}\n{QUEUE_HELPERS}\nwatchdog();
         let (s, port) = serve(&mut inst);
         println!(\"{{port}}\");
         accept(&mut inst, s);
         let out = inst.s_out();
         let (n, tail) = fill(&mut inst, out);
         println!(\"{{n}} {{tail}}\");
         inst.bye(0);"
    );
    let bin = crate::wasip2_typed_host::build_program(
        &ext_component(),
        &ext_core(),
        "TypedHost::new(Vec::new())",
        &body,
    );
    let mut child = std::process::Command::new(bin)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .unwrap();
    let mut lines = std::io::BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    lines.read_line(&mut line).unwrap();
    let port: u16 = line.trim().parse().unwrap();
    let mut client = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    line.clear();
    lines.read_line(&mut line).unwrap();
    let (n, tail) = line.trim().split_once(' ').unwrap();
    let mut expected = chunks(n.parse().unwrap());
    if tail == "true" {
        expected.extend(b"ping");
    }
    // the guest exits with the queue full; this peer starts reading late
    std::thread::sleep(std::time::Duration::from_millis(300));
    client
        .set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .unwrap();
    let mut got = Vec::new();
    client.read_to_end(&mut got).unwrap();
    assert!(
        got == expected,
        "{} bytes, expected {}",
        got.len(),
        expected.len()
    );
    assert!(child.wait().unwrap().success());
}

/// The same bytes `chunks` builds in the guest.
fn chunks(n: usize) -> Vec<u8> {
    let mut one = b"ping".to_vec();
    one.extend([0u8; 6]);
    one.extend(b"pong!");
    one.repeat(n)
}
