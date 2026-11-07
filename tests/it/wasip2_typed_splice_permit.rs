//! `splice` and `blocking-splice` to a TCP output stream respect `check-write`: the host reads
//! at most the permit from the source, so a stalled peer (a full send queue, permit 0) neither
//! hangs `splice` nor makes it consume input it cannot write. The guest is the socket guest of
//! `wasip2_typed_sockets` plus `splice`/`blocking_splice` (the count moved, or `-1 - the
//! stream-error case`); the source is the same connection's input stream, fed by a `std` peer.
//! Every body starts a watchdog, so a hang fails the test instead of stalling it.

use crate::wasip2_typed_host::{assert_succeeded, component_from_wit, run_program};
use crate::wasip2_typed_send_queue::QUEUE_HELPERS;
use crate::wasip2_typed_sockets::{CORE, HELPERS, IO, SOCKETS, WORLD};

/// `pattern(n)` mirrors the one of the splice tests; the rest (`watchdog`, `fill`, `top_off`,
/// `chunks`) is `QUEUE_HELPERS`.
const PATTERN: &str = r#"
#[allow(dead_code)]
fn pattern(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i % 251) as u8).collect()
}
"#;

fn io() -> String {
    IO.replace(
        "subscribe: func() -> pollable;\n  }\n  resource output-stream {",
        "subscribe: func() -> pollable;\n  }\n  resource output-stream {\n    splice: func(src: borrow<input-stream>, len: u64) -> result<u64, stream-error>;\n    blocking-splice: func(src: borrow<input-stream>, len: u64) -> result<u64, stream-error>;",
    )
}

fn core() -> String {
    CORE.replace(
        "(memory (export \"memory\") 1)",
        r#"(import "wasi:io/streams@0.2.0" "[method]output-stream.splice" (func $splice (param i32 i32 i64 i32)))
  (import "wasi:io/streams@0.2.0" "[method]output-stream.blocking-splice" (func $bsplice (param i32 i32 i64 i32)))
  (memory (export "memory") 1)
  (func (export "splice") (param $out i32) (param $in i32) (param $len i64) (result i64)
    (call $splice (local.get $out) (local.get $in) (local.get $len) (i32.const 600))
    (if (result i64) (i32.load8_u (i32.const 600))
      (then (i64.sub (i64.const -1) (i64.load8_u (i32.const 608))))
      (else (i64.load (i32.const 608)))))
  (func (export "blocking_splice") (param $out i32) (param $in i32) (param $len i64) (result i64)
    (call $bsplice (local.get $out) (local.get $in) (local.get $len) (i32.const 600))
    (if (result i64) (i32.load8_u (i32.const 600))
      (then (i64.sub (i64.const -1) (i64.load8_u (i32.const 608))))
      (else (i64.load (i32.const 608)))))"#,
    )
}

fn run(body: &str) -> std::process::Output {
    run_program(
        &component_from_wit(&[&io(), SOCKETS], WORLD, "w"),
        &core(),
        "TypedHost::new(Vec::new())",
        &format!("{HELPERS}\n{QUEUE_HELPERS}\n{PATTERN}\nwatchdog();\n{body}"),
    )
}

const SERVER: &str = "let (s, port) = serve(&mut inst);
         let client = std::net::TcpStream::connect((\"127.0.0.1\", port)).unwrap();
         accept(&mut inst, s);
         let (s_in, out) = (inst.s_in(), inst.s_out());";

const PEER_FIRST: &str = "#[allow(unused_imports)]
         use std::io::{Read, Write};
         let mut client = client;
         // the peer's bytes arrive first: they carry ACKs and window updates, which could let
         // the kernel take more of the queue after the fill
         let data = pattern(DATA_LEN);
         client.write_all(&data).unwrap();
         let sub = inst.sub_in(s_in);
         inst.block(sub);";

/// `body` runs after the peer's `DATA_LEN` bytes arrived (`data`, `s_in`, `out`, `client`).
fn run_peer_first(data_len: usize, body: &str) {
    assert_succeeded(&run(&format!(
        "{SERVER}
         const DATA_LEN: usize = {data_len};
         {PEER_FIRST}
         {body}"
    )));
}

#[test]
fn splice_to_a_full_queue_moves_nothing_and_keeps_the_input() {
    run_peer_first(
        100,
        "fill(&mut inst, out);
         top_off(&mut inst, out);
         // the kernel may still take bytes a moment later: only a permit that stays 0 counts
         std::thread::sleep(std::time::Duration::from_millis(200));
         assert_eq!(inst.check_write(out), 0);
         // the peer reads nothing: check-write is 0, so splice moves nothing
         assert_eq!(inst.splice(out, s_in, 4096), 0);
         assert_eq!(inst.check_write(out), 0);
         // and the input was not consumed
         assert_eq!(read_all(&mut inst, s_in, 100), data);
         drop(client);",
    );
}

#[test]
fn blocking_splice_waits_for_the_peer_and_keeps_the_order() {
    run_peer_first(
        100,
        "let (n, tail) = fill(&mut inst, out);
         let mut expected = chunks(n);
         if tail {
             expected.extend(b\"ping\");
         }
         // single bytes (the 'p' of `ping`) up to a permit of 0
         while inst.check_write(out) > 0 {
             assert_eq!(inst.write(out, 400, 1), 0);
             expected.push(b'p');
         }
         expected.extend(&data);
         // a peer that reads continuously, but only after 300 ms
         let reader = std::thread::spawn(move || {
             std::thread::sleep(std::time::Duration::from_millis(300));
             client.set_read_timeout(Some(std::time::Duration::from_secs(10))).unwrap();
             let mut got = vec![0u8; expected.len()];
             client.read_exact(&mut got).unwrap();
             assert!(got == expected, \"the bytes differ\");
         });
         let started = std::time::Instant::now();
         assert_eq!(inst.blocking_splice(out, s_in, 4096), 100);
         assert!(started.elapsed() >= std::time::Duration::from_millis(150), \"{:?}\", started.elapsed());
         // nothing is left in the host's queue for the peer to wait on
         assert_eq!(inst.flush(out), 0);
         reader.join().unwrap();",
    );
}

#[test]
fn splice_moves_at_most_the_permit() {
    run_peer_first(
        8192,
        "// the peer reads nothing: leave a room of 100 bytes in the queue (the bytes written
         // here are not checked)
         let mut permit = 0;
         for _ in 0..100_000 {
             permit = inst.check_write(out);
             if permit <= 100 {
                 // the kernel may take more of the queue a moment later: only a permit that
                 // stays put for 200 ms counts
                 std::thread::sleep(std::time::Duration::from_millis(200));
                 if inst.check_write(out) == permit {
                     break;
                 }
                 continue;
             }
             assert_eq!(inst.write(out, 400, (permit - 100) as i32), 0);
         }
         assert!((1..=100).contains(&permit), \"{permit}\");
         let moved = inst.splice(out, s_in, 4096);
         assert_eq!(moved, permit, \"moved {moved}, permit {permit}\");
         // the rest of the input is still there, in order
         let rest = read_all(&mut inst, s_in, DATA_LEN - moved as usize);
         assert_eq!(rest, data[moved as usize..].to_vec());
         drop(client);",
    );
}
