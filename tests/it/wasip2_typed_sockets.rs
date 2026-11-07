//! TCP over loopback (`wasi:sockets`) on the typed host, with the imports declared in WIT
//! text. The guest runs a client and a server in one instance; a few tests also talk to it
//! from the host side with `std::net`.

use crate::wasip2_typed_host::{assert_succeeded, component_from_wit, run_program};

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
    check-write: func() -> result<u64, stream-error>;
    write: func(contents: list<u8>) -> result<_, stream-error>;
    blocking-flush: func() -> result<_, stream-error>;
    subscribe: func() -> pollable;
  }
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
  use wasi:io/poll@0.2.0.{pollable};
  use network.{network, error-code, ip-socket-address};
  enum shutdown-type { receive, send, both }
  resource tcp-socket {
    shutdown: func(shutdown-type: shutdown-type) -> result<_, error-code>;
    start-connect: func(network: borrow<network>, remote-address: ip-socket-address) -> result<_, error-code>;
    finish-connect: func() -> result<tuple<input-stream, output-stream>, error-code>;
    accept: func() -> result<tuple<tcp-socket, input-stream, output-stream>, error-code>;
    remote-address: func() -> result<ip-socket-address, error-code>;
    subscribe: func() -> pollable;
    start-bind: func(network: borrow<network>, local-address: ip-socket-address) -> result<_, error-code>;
    finish-bind: func() -> result<_, error-code>;
    start-listen: func() -> result<_, error-code>;
    finish-listen: func() -> result<_, error-code>;
    local-address: func() -> result<ip-socket-address, error-code>;
  }
}
interface tcp-create-socket {
  use network.{network, error-code, ip-address-family};
  use tcp.{tcp-socket};
  create-tcp-socket: func(address-family: ip-address-family) -> result<tcp-socket, error-code>;
}
"#;

const WORLD: &str = r#"
package test:t;
world w {
  import wasi:io/streams@0.2.0;
  import wasi:sockets/network@0.2.0;
  import wasi:sockets/instance-network@0.2.0;
  import wasi:sockets/tcp@0.2.0;
  import wasi:sockets/tcp-create-socket@0.2.0;
}
"#;

const CORE: &str = r#"
(module
  (import "wasi:io/error@0.2.0" "[resource-drop]error" (func (param i32)))
  (import "wasi:io/poll@0.2.0" "[resource-drop]pollable" (func $drop_pollable (param i32)))
  (import "wasi:io/poll@0.2.0" "[method]pollable.block" (func $block (param i32)))
  (import "wasi:io/poll@0.2.0" "poll" (func $poll (param i32 i32 i32)))
  (import "wasi:io/streams@0.2.0" "[method]input-stream.subscribe" (func $sub_in (param i32) (result i32)))
  (import "wasi:io/streams@0.2.0" "[method]output-stream.subscribe" (func $sub_out (param i32) (result i32)))
  (import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.subscribe" (func $sub_sock (param i32) (result i32)))
  (import "wasi:io/streams@0.2.0" "[method]input-stream.read" (func $read (param i32 i64 i32)))
  (import "wasi:io/streams@0.2.0" "[resource-drop]input-stream" (func $drop_in (param i32)))
  (import "wasi:io/streams@0.2.0" "[method]output-stream.check-write" (func $check_write (param i32 i32)))
  (import "wasi:io/streams@0.2.0" "[method]output-stream.write" (func $write (param i32 i32 i32 i32)))
  (import "wasi:io/streams@0.2.0" "[method]output-stream.blocking-flush" (func $flush (param i32 i32)))
  (import "wasi:io/streams@0.2.0" "[resource-drop]output-stream" (func $drop_out (param i32)))
  (import "wasi:sockets/network@0.2.0" "[resource-drop]network" (func $drop_net (param i32)))
  (import "wasi:sockets/instance-network@0.2.0" "instance-network" (func $instance_network (result i32)))
  (import "wasi:sockets/tcp-create-socket@0.2.0" "create-tcp-socket" (func $create (param i32 i32)))
  (import "wasi:sockets/tcp@0.2.0" "[resource-drop]tcp-socket" (func $drop_sock (param i32)))
  (import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.start-bind"
    (func $start_bind (param i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32)))
  (import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.finish-bind" (func $finish_bind (param i32 i32)))
  (import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.start-listen" (func $start_listen (param i32 i32)))
  (import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.finish-listen" (func $finish_listen (param i32 i32)))
  (import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.local-address" (func $local (param i32 i32)))
  (import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.remote-address" (func $remote (param i32 i32)))
  (import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.shutdown" (func $shutdown (param i32 i32 i32)))
  (import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.start-connect"
    (func $start_connect (param i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32)))
  (import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.finish-connect" (func $finish_connect (param i32 i32)))
  (import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.accept" (func $accept (param i32 i32)))
  (memory (export "memory") 1)
  (global $brk (mut i32) (i32.const 1024))
  (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32) (local $p i32)
    (local.set $p (i32.and (i32.add (global.get $brk) (i32.sub (local.get 2) (i32.const 1)))
                           (i32.sub (i32.const 0) (local.get 2))))
    (global.set $brk (i32.add (local.get $p) (local.get 3)))
    (local.get $p))
  (global $net (mut i32) (i32.const -1))
  (global $c_in (mut i32) (i32.const -1))
  (global $c_out (mut i32) (i32.const -1))
  (global $s_sock (mut i32) (i32.const -1))
  (global $s_in (mut i32) (i32.const -1))
  (global $s_out (mut i32) (i32.const -1))
  (data (i32.const 400) "ping")
  (data (i32.const 410) "pong!")

  ;; result<_, error-code> at `at` (tag byte, then the error byte): 0, or 100 + the error code
  (func $status (param $at i32) (result i32)
    (if (result i32) (i32.load8_u (local.get $at))
      (then (i32.add (i32.const 100) (i32.load8_u (i32.add (local.get $at) (i32.const 1)))))
      (else (i32.const 0))))

  (func $network (result i32)
    (if (i32.lt_s (global.get $net) (i32.const 0)) (then (global.set $net (call $instance_network))))
    (global.get $net))
  (func (export "network") (result i32) (call $network))
  (func (export "drop_network") (call $drop_net (call $network)) (global.set $net (i32.const -1)))

  ;; create-tcp-socket(family): the handle, or -1 - the error code
  (func (export "create") (param $family i32) (result i32)
    (call $create (local.get $family) (i32.const 100))
    (if (result i32) (i32.load8_u (i32.const 100))
      (then (i32.sub (i32.const -1) (i32.load8_u (i32.const 104))))
      (else (i32.load (i32.const 104)))))
  (func (export "drop_socket") (param i32) (call $drop_sock (local.get 0)))

  ;; the address a.b.c.d:port (ipv4), as flattened ip-socket-address: tag 0, then
  ;; port a b c d and padding
  (func (export "bind") (param $s i32) (param $port i32) (param $a i32) (param $b i32) (param $c i32) (param $d i32) (result i32)
    (call $start_bind (local.get $s) (call $network) (i32.const 0)
      (local.get $port) (local.get $a) (local.get $b) (local.get $c) (local.get $d)
      (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0)
      (i32.const 120))
    (call $status (i32.const 120)))
  ;; the same for ::1:port (tag 1, port, flow info 0, eight segments, scope id 0)
  (func (export "bind6") (param $s i32) (param $port i32) (result i32)
    (call $start_bind (local.get $s) (call $network) (i32.const 1)
      (local.get $port) (i32.const 0)
      (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0)
      (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 1)
      (i32.const 0)
      (i32.const 120))
    (call $status (i32.const 120)))
  (func (export "finish_bind") (param i32) (result i32)
    (call $finish_bind (local.get 0) (i32.const 120)) (call $status (i32.const 120)))
  (func (export "start_listen") (param i32) (result i32)
    (call $start_listen (local.get 0) (i32.const 120)) (call $status (i32.const 120)))
  (func (export "finish_listen") (param i32) (result i32)
    (call $finish_listen (local.get 0) (i32.const 120)) (call $status (i32.const 120)))

  ;; an address result at 200: ipv4 as port | a << 16 | b << 24 | c << 32 | d << 40; ipv6 as
  ;; 1 << 48 | port | last segment << 16; or -1 - the error code
  (func $packed (result i64)
    (if (result i64) (i32.load8_u (i32.const 200))
      (then (i64.sub (i64.const -1) (i64.load8_u (i32.const 204))))
      (else
        (if (result i64) (i32.load8_u (i32.const 204))
          (then (i64.or (i64.const 0x1000000000000)
                  (i64.or (i64.load16_u (i32.const 208))
                          (i64.shl (i64.load16_u (i32.const 230)) (i64.const 16)))))
          (else (i64.or (i64.load16_u (i32.const 208))
                  (i64.or (i64.shl (i64.load8_u (i32.const 210)) (i64.const 16))
                  (i64.or (i64.shl (i64.load8_u (i32.const 211)) (i64.const 24))
                  (i64.or (i64.shl (i64.load8_u (i32.const 212)) (i64.const 32))
                          (i64.shl (i64.load8_u (i32.const 213)) (i64.const 40)))))))))))
  (func (export "local") (param $s i32) (result i64)
    (call $local (local.get $s) (i32.const 200)) (call $packed))
  (func (export "remote") (param $s i32) (result i64)
    (call $remote (local.get $s) (i32.const 200)) (call $packed))

  ;; result<_, error-code> or result<_, stream-error> at `at` whose error is at `at + 4`
  (func $status4 (param $at i32) (result i32)
    (if (result i32) (i32.load8_u (local.get $at))
      (then (i32.add (i32.const 100) (i32.load8_u (i32.add (local.get $at) (i32.const 4)))))
      (else (i32.const 0))))

  ;; start-connect to a.b.c.d:port (ipv4)
  (func (export "connect") (param $s i32) (param $port i32) (param $a i32) (param $b i32) (param $c i32) (param $d i32) (result i32)
    (call $start_connect (local.get $s) (call $network) (i32.const 0)
      (local.get $port) (local.get $a) (local.get $b) (local.get $c) (local.get $d)
      (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0)
      (i32.const 120))
    (call $status (i32.const 120)))
  ;; finish-connect: the streams are `c_in` and `c_out`
  (func (export "finish_connect") (param $s i32) (result i32)
    (call $finish_connect (local.get $s) (i32.const 140))
    (if (i32.eqz (i32.load8_u (i32.const 140)))
      (then (global.set $c_in (i32.load (i32.const 144)))
            (global.set $c_out (i32.load (i32.const 148)))))
    (call $status4 (i32.const 140)))
  ;; accept: the socket and its streams are `s_sock`, `s_in` and `s_out`
  (func (export "accept") (param $s i32) (result i32)
    (call $accept (local.get $s) (i32.const 160))
    (if (i32.eqz (i32.load8_u (i32.const 160)))
      (then (global.set $s_sock (i32.load (i32.const 164)))
            (global.set $s_in (i32.load (i32.const 168)))
            (global.set $s_out (i32.load (i32.const 172)))))
    (call $status4 (i32.const 160)))
  (func (export "c_in") (result i32) (global.get $c_in))
  (func (export "c_out") (result i32) (global.get $c_out))
  (func (export "s_sock") (result i32) (global.get $s_sock))
  (func (export "s_in") (result i32) (global.get $s_in))
  (func (export "s_out") (result i32) (global.get $s_out))

  ;; write the bytes at `ptr`: 0, or 100 + the stream-error case (1 is closed)
  (func (export "write") (param $h i32) (param $ptr i32) (param $len i32) (result i32)
    (call $write (local.get $h) (local.get $ptr) (local.get $len) (i32.const 180))
    (call $status4 (i32.const 180)))
  (func (export "flush") (param $h i32) (result i32)
    (call $flush (local.get $h) (i32.const 180)) (call $status4 (i32.const 180)))
  ;; check-write: the permitted length, or -1
  (func (export "check_write") (param $h i32) (result i64)
    (call $check_write (local.get $h) (i32.const 184))
    (if (result i64) (i32.load8_u (i32.const 184)) (then (i64.const -1)) (else (i64.load (i32.const 192)))))
  ;; read: the length of the list (0: nothing yet), or -1 - the stream-error case
  (func (export "read") (param $h i32) (param $len i64) (result i32)
    (call $read (local.get $h) (local.get $len) (i32.const 220))
    (if (result i32) (i32.load8_u (i32.const 220))
      (then (i32.sub (i32.const -1) (i32.load8_u (i32.const 224))))
      (else (i32.load (i32.const 228)))))
  ;; the i-th byte of the last list `read` returned
  (func (export "rbyte") (param $i i32) (result i32)
    (i32.load8_u (i32.add (i32.load (i32.const 224)) (local.get $i))))
  (func (export "sub_socket") (param i32) (result i32) (call $sub_sock (local.get 0)))
  (func (export "sub_in") (param i32) (result i32) (call $sub_in (local.get 0)))
  ;; a pollable that is always ready
  (func (export "sub_out") (param i32) (result i32) (call $sub_out (local.get 0)))
  (func (export "block") (param i32) (call $block (local.get 0)))
  (func (export "drop_pollable") (param i32) (call $drop_pollable (local.get 0)))
  ;; poll([a, b]): a bit mask of the positions in the list of ready ones
  (func (export "poll2") (param $a i32) (param $b i32) (result i32) (local $i i32) (local $mask i32)
    (i32.store (i32.const 500) (local.get $a))
    (i32.store (i32.const 504) (local.get $b))
    (call $poll (i32.const 500) (i32.const 2) (i32.const 512))
    (loop $next
      (if (i32.lt_u (local.get $i) (i32.load (i32.const 516)))
        (then
          (local.set $mask (i32.or (local.get $mask)
            (i32.shl (i32.const 1)
              (i32.load (i32.add (i32.load (i32.const 512)) (i32.shl (local.get $i) (i32.const 2)))))))
          (local.set $i (i32.add (local.get $i) (i32.const 1)))
          (br $next))))
    (local.get $mask))
  ;; shutdown(how): receive 0, send 1, both 2
  (func (export "shutdown") (param $s i32) (param $how i32) (result i32)
    (call $shutdown (local.get $s) (local.get $how) (i32.const 120)) (call $status (i32.const 120)))
  (func (export "drop_in") (param i32) (call $drop_in (local.get 0)))
  (func (export "drop_out") (param i32) (call $drop_out (local.get 0)))
)
"#;

fn run(body: &str) -> std::process::Output {
    run_with("TypedHost::new(Vec::new())", body)
}

/// Runs `body` with `HELPERS` in scope.
fn run_with_main(body: &str) -> std::process::Output {
    run(&format!("{HELPERS}\n{body}"))
}

fn run_with(host: &str, body: &str) -> std::process::Output {
    run_program(
        &component_from_wit(&[IO, SOCKETS], WORLD, "w"),
        CORE,
        host,
        body,
    )
}

#[test]
fn a_socket_binds_and_listens_on_loopback_and_reports_its_address() {
    assert_succeeded(&run("let s = inst.create(0);
         assert!(s >= 0, \"{s}\");
         assert_eq!(inst.bind(s, 0, 127, 0, 0, 1), 0);
         assert_eq!(inst.finish_bind(s), 0);
         let port = (inst.local(s) & 0xffff) as u16;
         assert_ne!(port, 0);
         assert_eq!(inst.local(s), i64::from(port) | 127 << 16 | 1 << 40);
         assert_eq!(inst.start_listen(s), 0);
         assert_eq!(inst.finish_listen(s), 0);
         // a host-side client can reach the listening port
         std::net::TcpStream::connect((\"127.0.0.1\", port)).unwrap();
         inst.drop_socket(s);"));
}

#[test]
fn an_ipv6_socket_binds_to_the_loopback_address() {
    assert_succeeded(&run("let s = inst.create(1);
         assert_eq!(inst.bind6(s, 0), 0);
         assert_eq!(inst.finish_bind(s), 0);
         let local = inst.local(s);
         assert_ne!(local & 0xffff, 0);
         // an ipv6 address whose last segment is 1
         assert_eq!(local >> 48, 1);
         assert_eq!((local >> 16) & 0xffff, 1);
         // an ipv4 address does not fit an ipv6 socket (invalid-argument = 3)
         let t = inst.create(1);
         assert_eq!(inst.bind(t, 0, 127, 0, 0, 1), 103);"));
}

#[test]
fn an_address_outside_loopback_is_refused_unless_the_host_allows_the_network() {
    // access-denied is 1; 0.0.0.0 is not loopback and binding it needs no outside network
    let body = |expected: i32| {
        format!("let s = inst.create(0); assert_eq!(inst.bind(s, 0, 0, 0, 0, 0), {expected});")
    };
    assert_succeeded(&run(&body(101)));
    assert_succeeded(&run_with(
        "TypedHost::new(Vec::new()).with_external_network()",
        &body(0),
    ));
}

#[test]
fn a_socket_used_out_of_order_reports_its_state() {
    // not-in-progress is 7, invalid-state 9
    assert_succeeded(&run("let s = inst.create(0);
         assert_eq!(inst.finish_bind(s), 107);
         assert_eq!(inst.start_listen(s), 109);
         assert_eq!(inst.finish_listen(s), 107);
         assert_eq!(inst.bind(s, 0, 127, 0, 0, 1), 0);
         // a second start-bind while one is in progress
         assert_eq!(inst.bind(s, 0, 127, 0, 0, 1), 109);
         // an unbound socket has no local address (invalid-state, as -1 - 9)
         let t = inst.create(0);
         assert_eq!(inst.local(t), -10);"));
}

#[test]
fn a_dropped_socket_frees_its_handle_for_the_next_one() {
    assert_succeeded(&run("let a = inst.create(0);
         let b = inst.create(0);
         assert_ne!(a, b);
         inst.drop_socket(a);
         assert_eq!(inst.create(0), a);
         // the network handle can be dropped and asked for again
         inst.network();
         inst.drop_network();
         assert_eq!(inst.network(), 0);"));
}

/// Guest-side helpers for the bodies of the tests below: `serve` makes a listening socket
/// and returns its port; `read_all` polls `read` until `n` bytes arrived.
const HELPERS: &str = r#"
#[allow(dead_code)]
fn serve(inst: &mut Instance<Glue<TypedHost>>) -> (i32, u16) {
    let s = inst.create(0);
    assert_eq!(inst.bind(s, 0, 127, 0, 0, 1), 0);
    assert_eq!(inst.finish_bind(s), 0);
    assert_eq!(inst.start_listen(s), 0);
    assert_eq!(inst.finish_listen(s), 0);
    (s, (inst.local(s) & 0xffff) as u16)
}
#[allow(dead_code)]
fn accept(inst: &mut Instance<Glue<TypedHost>>, s: i32) {
    for _ in 0..2000 {
        match inst.accept(s) {
            0 => return,
            108 => std::thread::sleep(std::time::Duration::from_millis(1)),
            other => panic!("accept: {other}"),
        }
    }
    panic!("accept: nothing arrived");
}
#[allow(dead_code)]
fn read_all(inst: &mut Instance<Glue<TypedHost>>, h: i32, n: usize) -> Vec<u8> {
    let mut got = Vec::new();
    for _ in 0..2000 {
        let len = inst.read(h, 4096);
        assert!(len >= 0, "read: {len}");
        got.extend((0..len).map(|i| inst.rbyte(i) as u8));
        if got.len() >= n {
            return got;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    panic!("read: only {got:?}");
}
"#;

#[test]
fn a_client_and_a_server_in_one_guest_exchange_bytes() {
    assert_succeeded(&run_with_main(
        "let (s, port) = serve(&mut inst);
         let c = inst.create(0);
         assert_eq!(inst.connect(c, port.into(), 127, 0, 0, 1), 0);
         assert_eq!(inst.finish_connect(c), 0);
         accept(&mut inst, s);
         let (c_in, c_out, s_in, s_out) = (inst.c_in(), inst.c_out(), inst.s_in(), inst.s_out());
         // the streams are far from stdin, stdout, stderr and the file streams (an input and an
         // output stream are in separate handle spaces, so may have the same number)
         assert!([c_in, c_out, s_in, s_out].iter().all(|&h| h >= 1 << 30));
         assert_ne!(c_in, s_in);
         assert!(inst.check_write(c_out) > 0);
         assert_eq!(inst.write(c_out, 400, 4), 0);
         assert_eq!(inst.flush(c_out), 0);
         assert_eq!(read_all(&mut inst, s_in, 4), b\"ping\");
         assert_eq!(inst.write(s_out, 410, 5), 0);
         assert_eq!(read_all(&mut inst, c_in, 5), b\"pong!\");
         // each end sees the other's address
         assert_eq!(inst.remote(c) & 0xffff, i64::from(port));
         let conn = inst.s_sock();
         assert_eq!(inst.remote(conn), inst.local(c));
         // a listener has no peer (invalid-state, as -1 - 9)
         assert_eq!(inst.remote(s), -10);",
    ));
}

#[test]
fn accept_with_no_pending_connection_would_block() {
    // would-block is 8
    assert_succeeded(&run_with_main(
        "let (s, _) = serve(&mut inst);
         assert_eq!(inst.accept(s), 108);
         // a socket that is not listening cannot accept (invalid-state 9)
         let t = inst.create(0);
         assert_eq!(inst.accept(t), 109);",
    ));
}

#[test]
fn a_host_side_client_talks_to_a_guest_server_until_it_hangs_up() {
    assert_succeeded(&run_with_main(
        "use std::io::{Read, Write};
         let (s, port) = serve(&mut inst);
         let mut client = std::net::TcpStream::connect((\"127.0.0.1\", port)).unwrap();
         client.write_all(b\"hello\").unwrap();
         accept(&mut inst, s);
         let (s_in, s_out) = (inst.s_in(), inst.s_out());
         assert_eq!(read_all(&mut inst, s_in, 5), b\"hello\");
         assert_eq!(inst.write(s_out, 410, 4), 0);
         let mut reply = [0u8; 4];
         client.read_exact(&mut reply).unwrap();
         assert_eq!(&reply, b\"pong\");
         // the client hangs up: the end of the input is `closed` (stream-error case 1)
         drop(client);
         let mut ended = false;
         for _ in 0..2000 {
             if inst.read(s_in, 16) == -2 { ended = true; break; }
             std::thread::sleep(std::time::Duration::from_millis(1));
         }
         assert!(ended);",
    ));
}

#[test]
fn a_connection_to_a_closed_port_is_refused() {
    // connection-refused is 14
    assert_succeeded(&run_with_main(
        "let port = {
             let l = std::net::TcpListener::bind((\"127.0.0.1\", 0)).unwrap();
             l.local_addr().unwrap().port()
         };
         let c = inst.create(0);
         assert_eq!(inst.connect(c, port.into(), 127, 0, 0, 1), 0);
         assert_eq!(inst.finish_connect(c), 114);
         // a second finish-connect has nothing in progress (not-in-progress 7)
         assert_eq!(inst.finish_connect(c), 107);
         // outside loopback the connect is refused up front (access-denied 1)
         let d = inst.create(0);
         assert_eq!(inst.connect(d, 80, 192, 0, 2, 1), 101);
         // a bound socket cannot connect (not-supported 2)
         let b = inst.create(0);
         assert_eq!(inst.bind(b, 0, 127, 0, 0, 1), 0);
         assert_eq!(inst.connect(b, 80, 127, 0, 0, 1), 102);",
    ));
}

#[test]
fn dropping_connection_streams_and_sockets_frees_their_slots() {
    assert_succeeded(&run_with_main(
        "let (s, port) = serve(&mut inst);
         let _client = std::net::TcpStream::connect((\"127.0.0.1\", port)).unwrap();
         accept(&mut inst, s);
         let (conn, i, o) = (inst.s_sock(), inst.s_in(), inst.s_out());
         inst.drop_in(i);
         inst.drop_out(o);
         inst.drop_socket(conn);
         // the next connection reuses all three slots
         let _second = std::net::TcpStream::connect((\"127.0.0.1\", port)).unwrap();
         accept(&mut inst, s);
         assert_eq!((inst.s_sock(), inst.s_in(), inst.s_out()), (conn, i, o));",
    ));
}

#[test]
fn a_listening_socket_is_ready_once_a_connection_is_pending() {
    assert_succeeded(&run_with_main(
        "let (s, port) = serve(&mut inst);
         let listening = inst.sub_socket(s);
         let always = inst.sub_out(1);
         // only the second pollable is ready
         assert_eq!(inst.poll2(listening, always), 0b10);
         let _client = std::net::TcpStream::connect((\"127.0.0.1\", port)).unwrap();
         // `block` returns once the connection is there, and so does `accept`
         inst.block(listening);
         assert_eq!(inst.poll2(listening, always), 0b11);
         assert_eq!(inst.accept(s), 0);
         // nothing pending again
         assert_eq!(inst.poll2(listening, always), 0b10);
         inst.drop_pollable(listening);
         inst.drop_pollable(always);",
    ));
}

#[test]
fn a_connections_input_stream_is_ready_with_data_or_at_its_end() {
    assert_succeeded(&run_with_main(
        "use std::io::Write;
         let (s, port) = serve(&mut inst);
         let mut client = std::net::TcpStream::connect((\"127.0.0.1\", port)).unwrap();
         accept(&mut inst, s);
         let (s_in, s_out) = (inst.s_in(), inst.s_out());
         let readable = inst.sub_in(s_in);
         let always = inst.sub_out(s_out);
         assert_eq!(inst.poll2(readable, always), 0b10);
         client.write_all(b\"data\").unwrap();
         inst.block(readable);
         assert_eq!(inst.poll2(readable, always), 0b11);
         assert_eq!(read_all(&mut inst, s_in, 4), b\"data\");
         assert_eq!(inst.poll2(readable, always), 0b10);
         // the end of the stream makes it ready, too
         drop(client);
         inst.block(readable);
         assert_eq!(inst.read(s_in, 8), -2);",
    ));
}

#[test]
fn shutdown_of_the_send_side_ends_the_peers_input_but_not_the_other_direction() {
    assert_succeeded(&run_with_main(
        "let (s, port) = serve(&mut inst);
         let c = inst.create(0);
         assert_eq!(inst.connect(c, port.into(), 127, 0, 0, 1), 0);
         assert_eq!(inst.finish_connect(c), 0);
         accept(&mut inst, s);
         let (c_in, c_out, s_in, s_out) = (inst.c_in(), inst.c_out(), inst.s_in(), inst.s_out());
         assert_eq!(inst.write(c_out, 400, 4), 0);
         // shutdown(send) (1): the server reads the data, then the end of the stream
         assert_eq!(inst.shutdown(c, 1), 0);
         assert_eq!(read_all(&mut inst, s_in, 4), b\"ping\");
         let mut ended = false;
         for _ in 0..2000 {
             if inst.read(s_in, 16) == -2 { ended = true; break; }
             std::thread::sleep(std::time::Duration::from_millis(1));
         }
         assert!(ended);
         // the other direction still works
         assert_eq!(inst.write(s_out, 410, 5), 0);
         assert_eq!(read_all(&mut inst, c_in, 5), b\"pong!\");
         // shutdown(both) (2) of a second connection ends the peer's input, too
         let d = inst.create(0);
         assert_eq!(inst.connect(d, port.into(), 127, 0, 0, 1), 0);
         assert_eq!(inst.finish_connect(d), 0);
         accept(&mut inst, s);
         let d_in = inst.s_in();
         assert_eq!(inst.shutdown(d, 2), 0);
         let mut ended = false;
         for _ in 0..2000 {
             if inst.read(d_in, 16) == -2 { ended = true; break; }
             std::thread::sleep(std::time::Duration::from_millis(1));
         }
         assert!(ended);
         // a listener cannot be shut down (invalid-state 9)
         assert_eq!(inst.shutdown(s, 2), 109);
         let t = inst.create(0);
         assert_eq!(inst.shutdown(t, 0), 109);",
    ));
}
