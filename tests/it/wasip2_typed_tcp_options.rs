//! The TCP socket options on the typed host: recorded per socket with the spec's defaults,
//! validated, and read back (they are not applied to the OS socket). The component does not
//! lower `[resource-drop]network`, as a real `std::net` program does not.

use crate::wasip2_typed_host::{assert_succeeded, run_program};

/// The component of `WORLD` around `CORE`: `wit-component` lowers only the functions the core
/// imports, so no `[resource-drop]` is declared for the resources it never drops.
fn component() -> Vec<u8> {
    use wit_component::{ComponentEncoder, StringEncoding, embed_component_metadata};
    let mut resolve = wit_parser::Resolve::default();
    resolve.push_str("io.wit", IO).unwrap();
    resolve.push_str("sockets.wit", SOCKETS).unwrap();
    let package = resolve.push_str("test.wit", WORLD).unwrap();
    let world = resolve.select_world(&[package], Some("w")).unwrap();
    let mut module = wat::parse_str(CORE).unwrap();
    embed_component_metadata(&mut module, &resolve, world, StringEncoding::UTF8, false).unwrap();
    ComponentEncoder::default()
        .module(&module)
        .unwrap()
        .validate(true)
        .encode()
        .unwrap()
}

const IO: &str = r#"
package wasi:io@0.2.0;
interface streams {
  resource input-stream;
  resource output-stream;
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
  type duration = u64;
  resource tcp-socket {
    start-connect: func(network: borrow<network>, remote-address: ip-socket-address) -> result<_, error-code>;
    accept: func() -> result<tuple<tcp-socket, input-stream, output-stream>, error-code>;
    local-address: func() -> result<ip-socket-address, error-code>;
    start-bind: func(network: borrow<network>, local-address: ip-socket-address) -> result<_, error-code>;
    finish-bind: func() -> result<_, error-code>;
    start-listen: func() -> result<_, error-code>;
    finish-listen: func() -> result<_, error-code>;
    is-listening: func() -> bool;
    set-listen-backlog-size: func(value: u64) -> result<_, error-code>;
    keep-alive-enabled: func() -> result<bool, error-code>;
    set-keep-alive-enabled: func(value: bool) -> result<_, error-code>;
    keep-alive-idle-time: func() -> result<duration, error-code>;
    set-keep-alive-idle-time: func(value: duration) -> result<_, error-code>;
    keep-alive-interval: func() -> result<duration, error-code>;
    set-keep-alive-interval: func(value: duration) -> result<_, error-code>;
    keep-alive-count: func() -> result<u32, error-code>;
    set-keep-alive-count: func(value: u32) -> result<_, error-code>;
    hop-limit: func() -> result<u8, error-code>;
    set-hop-limit: func(value: u8) -> result<_, error-code>;
    receive-buffer-size: func() -> result<u64, error-code>;
    set-receive-buffer-size: func(value: u64) -> result<_, error-code>;
    send-buffer-size: func() -> result<u64, error-code>;
    set-send-buffer-size: func(value: u64) -> result<_, error-code>;
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
  import wasi:sockets/instance-network@0.2.0;
  import wasi:sockets/tcp@0.2.0;
  import wasi:sockets/tcp-create-socket@0.2.0;
}
"#;

/// Exports `create`, `bind_listen` (loopback, port 0: 0 or 100 + error code, stopping at the
/// first failure), `start_listen`, `is_listening`, and a getter and a setter per option. A
/// getter returns the value, or -1 - the error code; a setter 0 or 100 + the error code.
const CORE: &str = r#"
(module
  (import "wasi:sockets/instance-network@0.2.0" "instance-network" (func $instance_network (result i32)))
  (import "wasi:sockets/tcp-create-socket@0.2.0" "create-tcp-socket" (func $create (param i32 i32)))
  (import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.start-bind"
    (func $start_bind (param i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32)))
  (import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.start-connect"
    (func $start_connect (param i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32)))
  (import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.accept" (func $accept (param i32 i32)))
  (import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.local-address" (func $local (param i32 i32)))
  (import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.finish-bind" (func $finish_bind (param i32 i32)))
  (import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.start-listen" (func $start_listen (param i32 i32)))
  (import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.finish-listen" (func $finish_listen (param i32 i32)))
  (import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.is-listening" (func $is_listening (param i32) (result i32)))
  (import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.set-listen-backlog-size" (func $set_backlog (param i32 i64 i32)))
  (import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.keep-alive-enabled" (func $ka_enabled (param i32 i32)))
  (import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.set-keep-alive-enabled" (func $set_ka_enabled (param i32 i32 i32)))
  (import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.keep-alive-idle-time" (func $ka_idle (param i32 i32)))
  (import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.set-keep-alive-idle-time" (func $set_ka_idle (param i32 i64 i32)))
  (import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.keep-alive-interval" (func $ka_interval (param i32 i32)))
  (import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.set-keep-alive-interval" (func $set_ka_interval (param i32 i64 i32)))
  (import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.keep-alive-count" (func $ka_count (param i32 i32)))
  (import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.set-keep-alive-count" (func $set_ka_count (param i32 i32 i32)))
  (import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.hop-limit" (func $hop (param i32 i32)))
  (import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.set-hop-limit" (func $set_hop (param i32 i32 i32)))
  (import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.receive-buffer-size" (func $rcv (param i32 i32)))
  (import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.set-receive-buffer-size" (func $set_rcv (param i32 i64 i32)))
  (import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.send-buffer-size" (func $snd (param i32 i32)))
  (import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.set-send-buffer-size" (func $set_snd (param i32 i64 i32)))
  (memory (export "memory") 1)
  (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32) (i32.const 0))

  ;; result<_, error-code> at `at`: 0, or 100 + the error
  (func $status (param $at i32) (result i32)
    (if (result i32) (i32.load8_u (local.get $at))
      (then (i32.add (i32.const 100) (i32.load8_u (i32.add (local.get $at) (i32.const 1)))))
      (else (i32.const 0))))
  ;; a getter's result at 64 whose payload is at `off`: the value, or -1 - the error
  (func $value (param $off i32) (result i64)
    (if (result i64) (i32.load8_u (i32.const 64))
      (then (i64.sub (i64.const -1) (i64.load8_u (i32.add (i32.const 64) (local.get $off)))))
      (else (i64.load (i32.add (i32.const 64) (local.get $off))))))

  (func (export "create") (param $family i32) (result i32)
    (call $create (local.get $family) (i32.const 100))
    (i32.load (i32.const 104)))
  (func (export "bind_listen") (param $s i32) (result i32) (local $r i32)
    (call $start_bind (local.get $s) (call $instance_network) (i32.const 0)
      (i32.const 0) (i32.const 127) (i32.const 0) (i32.const 0) (i32.const 1)
      (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0)
      (i32.const 120))
    (local.set $r (call $status (i32.const 120)))
    (if (local.get $r) (then (return (local.get $r))))
    (call $finish_bind (local.get $s) (i32.const 120))
    (call $status (i32.const 120)))
  ;; the port a bound ipv4 socket got
  (func (export "port") (param i32) (result i32)
    (call $local (local.get 0) (i32.const 200)) (i32.load16_u (i32.const 208)))
  ;; start-connect to 127.0.0.1:port: 0 or 100 + the error
  (func (export "connect") (param $s i32) (param $port i32) (result i32)
    (call $start_connect (local.get $s) (call $instance_network) (i32.const 0)
      (local.get $port) (i32.const 127) (i32.const 0) (i32.const 0) (i32.const 1)
      (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0)
      (i32.const 120))
    (call $status (i32.const 120)))
  ;; accept: the new socket's handle, or -1 - the error
  (func (export "accept") (param $s i32) (result i32)
    (call $accept (local.get $s) (i32.const 140))
    (if (result i32) (i32.load8_u (i32.const 140))
      (then (i32.sub (i32.const -1) (i32.load8_u (i32.const 144))))
      (else (i32.load (i32.const 144)))))
  (func (export "start_listen") (param i32) (result i32)
    (call $start_listen (local.get 0) (i32.const 120)) (call $status (i32.const 120)))
  (func (export "finish_listen") (param i32) (result i32)
    (call $finish_listen (local.get 0) (i32.const 120)) (call $status (i32.const 120)))
  (func (export "is_listening") (param i32) (result i32) (call $is_listening (local.get 0)))

  (func (export "set_backlog") (param i32 i64) (result i32)
    (call $set_backlog (local.get 0) (local.get 1) (i32.const 120)) (call $status (i32.const 120)))
  (func (export "ka_enabled") (param i32) (result i64)
    (call $ka_enabled (local.get 0) (i32.const 64))
    (if (result i64) (i32.load8_u (i32.const 64))
      (then (i64.sub (i64.const -1) (i64.load8_u (i32.const 65))))
      (else (i64.load8_u (i32.const 65)))))
  (func (export "set_ka_enabled") (param i32 i32) (result i32)
    (call $set_ka_enabled (local.get 0) (local.get 1) (i32.const 120)) (call $status (i32.const 120)))
  (func (export "ka_idle") (param i32) (result i64)
    (call $ka_idle (local.get 0) (i32.const 64)) (call $value (i32.const 8)))
  (func (export "set_ka_idle") (param i32 i64) (result i32)
    (call $set_ka_idle (local.get 0) (local.get 1) (i32.const 120)) (call $status (i32.const 120)))
  (func (export "ka_interval") (param i32) (result i64)
    (call $ka_interval (local.get 0) (i32.const 64)) (call $value (i32.const 8)))
  (func (export "set_ka_interval") (param i32 i64) (result i32)
    (call $set_ka_interval (local.get 0) (local.get 1) (i32.const 120)) (call $status (i32.const 120)))
  (func (export "ka_count") (param i32) (result i64)
    (call $ka_count (local.get 0) (i32.const 64))
    (if (result i64) (i32.load8_u (i32.const 64))
      (then (i64.sub (i64.const -1) (i64.load8_u (i32.const 68))))
      (else (i64.load32_u (i32.const 68)))))
  (func (export "set_ka_count") (param i32 i32) (result i32)
    (call $set_ka_count (local.get 0) (local.get 1) (i32.const 120)) (call $status (i32.const 120)))
  (func (export "hop") (param i32) (result i64)
    (call $hop (local.get 0) (i32.const 64))
    (if (result i64) (i32.load8_u (i32.const 64))
      (then (i64.sub (i64.const -1) (i64.load8_u (i32.const 65))))
      (else (i64.load8_u (i32.const 65)))))
  (func (export "set_hop") (param i32 i32) (result i32)
    (call $set_hop (local.get 0) (local.get 1) (i32.const 120)) (call $status (i32.const 120)))
  (func (export "rcv") (param i32) (result i64)
    (call $rcv (local.get 0) (i32.const 64)) (call $value (i32.const 8)))
  (func (export "set_rcv") (param i32 i64) (result i32)
    (call $set_rcv (local.get 0) (local.get 1) (i32.const 120)) (call $status (i32.const 120)))
  (func (export "snd") (param i32) (result i64)
    (call $snd (local.get 0) (i32.const 64)) (call $value (i32.const 8)))
  (func (export "set_snd") (param i32 i64) (result i32)
    (call $set_snd (local.get 0) (local.get 1) (i32.const 120)) (call $status (i32.const 120)))
)
"#;

fn run(body: &str) -> std::process::Output {
    run_program(&component(), CORE, "TypedHost::new(Vec::new())", body)
}

const INVALID_ARGUMENT: i32 = 103;
const INVALID_STATE: i32 = 109;

#[test]
fn a_new_socket_has_the_spec_defaults() {
    assert_succeeded(&run("let s = inst.create(0);
         assert_eq!(inst.is_listening(s), 0);
         assert_eq!(inst.ka_enabled(s), 0);
         assert_eq!(inst.ka_idle(s), 7200_000_000_000);
         assert_eq!(inst.ka_interval(s), 75_000_000_000);
         assert_eq!(inst.ka_count(s), 9);
         assert_eq!(inst.hop(s), 64);
         assert!(inst.rcv(s) > 0 && inst.snd(s) > 0);"));
}

#[test]
fn a_value_set_is_the_value_read_back_per_socket() {
    assert_succeeded(&run("let a = inst.create(0);
         let b = inst.create(1);
         assert_eq!(inst.set_ka_enabled(a, 1), 0);
         assert_eq!(inst.set_ka_idle(a, 30_000_000_000), 0);
         assert_eq!(inst.set_ka_interval(a, 5_000_000_000), 0);
         assert_eq!(inst.set_ka_count(a, 3), 0);
         assert_eq!(inst.set_hop(a, 7), 0);
         assert_eq!(inst.set_rcv(a, 12345), 0);
         assert_eq!(inst.set_snd(a, 54321), 0);
         assert_eq!(inst.ka_enabled(a), 1);
         assert_eq!(inst.ka_idle(a), 30_000_000_000);
         assert_eq!(inst.ka_interval(a), 5_000_000_000);
         assert_eq!(inst.ka_count(a), 3);
         assert_eq!(inst.hop(a), 7);
         assert_eq!(inst.rcv(a), 12345);
         assert_eq!(inst.snd(a), 54321);
         // another socket is untouched
         assert_eq!(inst.ka_enabled(b), 0);
         assert_eq!(inst.hop(b), 64);"));
}

#[test]
fn zero_is_an_invalid_argument_and_leaves_the_value() {
    assert_succeeded(&run(&format!(
        "let s = inst.create(0);
         assert_eq!(inst.set_hop(s, 0), {INVALID_ARGUMENT});
         assert_eq!(inst.set_ka_idle(s, 0), {INVALID_ARGUMENT});
         assert_eq!(inst.set_ka_interval(s, 0), {INVALID_ARGUMENT});
         assert_eq!(inst.set_ka_count(s, 0), {INVALID_ARGUMENT});
         assert_eq!(inst.set_rcv(s, 0), {INVALID_ARGUMENT});
         assert_eq!(inst.set_snd(s, 0), {INVALID_ARGUMENT});
         assert_eq!(inst.set_backlog(s, 0), {INVALID_ARGUMENT});
         assert_eq!(inst.hop(s), 64);
         assert_eq!(inst.ka_count(s), 9);"
    )));
}

#[test]
fn is_listening_is_true_once_listening_has_started() {
    assert_succeeded(&run("let s = inst.create(0);
         assert_eq!(inst.bind_listen(s), 0);
         assert_eq!(inst.is_listening(s), 0);
         assert_eq!(inst.start_listen(s), 0);
         assert_eq!(inst.finish_listen(s), 0);
         assert_eq!(inst.is_listening(s), 1);"));
}

#[test]
fn the_backlog_size_is_accepted_until_a_connection_is_made() {
    assert_succeeded(&run(&format!(
        "let s = inst.create(0);
         assert_eq!(inst.set_backlog(s, 16), 0);
         assert_eq!(inst.bind_listen(s), 0);
         assert_eq!(inst.set_backlog(s, 32), 0);
         assert_eq!(inst.start_listen(s), 0);
         assert_eq!(inst.finish_listen(s), 0);
         // while listening it is ignored, not an error
         assert_eq!(inst.set_backlog(s, 64), 0);
         let c = inst.create(0);
         let port = inst.port(s);
         assert_eq!(inst.connect(c, port), 0);
         assert_eq!(inst.set_backlog(c, 64), {INVALID_STATE});"
    )));
}

#[test]
fn an_accepted_socket_inherits_the_listeners_options() {
    assert_succeeded(&run("let l = inst.create(0);
         assert_eq!(inst.set_ka_count(l, 3), 0);
         assert_eq!(inst.set_hop(l, 7), 0);
         assert_eq!(inst.set_rcv(l, 12345), 0);
         assert_eq!(inst.bind_listen(l), 0);
         assert_eq!(inst.start_listen(l), 0);
         assert_eq!(inst.finish_listen(l), 0);
         let c = inst.create(0);
         let port = inst.port(l);
         assert_eq!(inst.connect(c, port), 0);
         // the connection may not be queued yet: would-block (-1 - 8) means try again
         let mut a = inst.accept(l);
         for _ in 0..2000 {
             if a != -9 {
                 break;
             }
             std::thread::sleep(std::time::Duration::from_millis(1));
             a = inst.accept(l);
         }
         assert!(a >= 0, \"{a}\");
         assert_eq!(inst.ka_count(a), 3);
         assert_eq!(inst.hop(a), 7);
         assert_eq!(inst.rcv(a), 12345);
         assert_eq!(inst.snd(a), 65536);"));
}
