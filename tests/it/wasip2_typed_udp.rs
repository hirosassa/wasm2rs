//! UDP over loopback (`wasi:sockets` udp, udp-create-socket) on the typed host, with the
//! imports declared in WIT text. The guest makes several sockets in one instance and
//! sends datagrams between them.

use crate::wasip2_typed_host::{assert_succeeded, component_from_wit, run_program};

const IO: &str = r#"
package wasi:io@0.2.0;
interface poll {
  resource pollable {
    block: func();
  }
  poll: func(in: list<borrow<pollable>>) -> list<u32>;
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
interface udp {
  use wasi:io/poll@0.2.0.{pollable};
  use network.{network, error-code, ip-socket-address, ip-address-family};
  record incoming-datagram { data: list<u8>, remote-address: ip-socket-address }
  record outgoing-datagram { data: list<u8>, remote-address: option<ip-socket-address> }
  resource udp-socket {
    start-bind: func(network: borrow<network>, local-address: ip-socket-address) -> result<_, error-code>;
    finish-bind: func() -> result<_, error-code>;
    %stream: func(remote-address: option<ip-socket-address>) -> result<tuple<incoming-datagram-stream, outgoing-datagram-stream>, error-code>;
    local-address: func() -> result<ip-socket-address, error-code>;
    remote-address: func() -> result<ip-socket-address, error-code>;
    address-family: func() -> ip-address-family;
    ipv6-only: func() -> result<bool, error-code>;
    set-ipv6-only: func(value: bool) -> result<_, error-code>;
    unicast-hop-limit: func() -> result<u8, error-code>;
    set-unicast-hop-limit: func(value: u8) -> result<_, error-code>;
    receive-buffer-size: func() -> result<u64, error-code>;
    set-receive-buffer-size: func(value: u64) -> result<_, error-code>;
    send-buffer-size: func() -> result<u64, error-code>;
    set-send-buffer-size: func(value: u64) -> result<_, error-code>;
    subscribe: func() -> pollable;
  }
  resource incoming-datagram-stream {
    receive: func(max-results: u64) -> result<list<incoming-datagram>, error-code>;
    subscribe: func() -> pollable;
  }
  resource outgoing-datagram-stream {
    check-send: func() -> result<u64, error-code>;
    send: func(datagrams: list<outgoing-datagram>) -> result<u64, error-code>;
    subscribe: func() -> pollable;
  }
}
interface udp-create-socket {
  use network.{network, error-code, ip-address-family};
  use udp.{udp-socket};
  create-udp-socket: func(address-family: ip-address-family) -> result<udp-socket, error-code>;
}
"#;

const WORLD: &str = r#"
package test:t;
world w {
  import wasi:io/poll@0.2.0;
  import wasi:sockets/network@0.2.0;
  import wasi:sockets/instance-network@0.2.0;
  import wasi:sockets/udp@0.2.0;
  import wasi:sockets/udp-create-socket@0.2.0;
}
"#;

const CORE: &str = r#"
(module
  (import "wasi:io/poll@0.2.0" "[resource-drop]pollable" (func $drop_pollable (param i32)))
  (import "wasi:io/poll@0.2.0" "[method]pollable.block" (func $block (param i32)))
  (import "wasi:io/poll@0.2.0" "poll" (func $poll (param i32 i32 i32)))
  (import "wasi:sockets/network@0.2.0" "[resource-drop]network" (func $drop_net (param i32)))
  (import "wasi:sockets/instance-network@0.2.0" "instance-network" (func $instance_network (result i32)))
  (import "wasi:sockets/udp-create-socket@0.2.0" "create-udp-socket" (func $create (param i32 i32)))
  (import "wasi:sockets/udp@0.2.0" "[resource-drop]udp-socket" (func $drop_sock (param i32)))
  (import "wasi:sockets/udp@0.2.0" "[resource-drop]incoming-datagram-stream" (func $drop_in (param i32)))
  (import "wasi:sockets/udp@0.2.0" "[resource-drop]outgoing-datagram-stream" (func $drop_out (param i32)))
  (import "wasi:sockets/udp@0.2.0" "[method]udp-socket.start-bind"
    (func $start_bind (param i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32)))
  (import "wasi:sockets/udp@0.2.0" "[method]udp-socket.finish-bind" (func $finish_bind (param i32 i32)))
  (import "wasi:sockets/udp@0.2.0" "[method]udp-socket.stream"
    (func $stream (param i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32)))
  (import "wasi:sockets/udp@0.2.0" "[method]udp-socket.local-address" (func $local (param i32 i32)))
  (import "wasi:sockets/udp@0.2.0" "[method]udp-socket.remote-address" (func $remote (param i32 i32)))
  (import "wasi:sockets/udp@0.2.0" "[method]udp-socket.address-family" (func $family (param i32) (result i32)))
  (import "wasi:sockets/udp@0.2.0" "[method]udp-socket.ipv6-only" (func $ipv6_only (param i32 i32)))
  (import "wasi:sockets/udp@0.2.0" "[method]udp-socket.set-ipv6-only" (func $set_ipv6_only (param i32 i32 i32)))
  (import "wasi:sockets/udp@0.2.0" "[method]udp-socket.unicast-hop-limit" (func $hop (param i32 i32)))
  (import "wasi:sockets/udp@0.2.0" "[method]udp-socket.set-unicast-hop-limit" (func $set_hop (param i32 i32 i32)))
  (import "wasi:sockets/udp@0.2.0" "[method]udp-socket.receive-buffer-size" (func $rbuf (param i32 i32)))
  (import "wasi:sockets/udp@0.2.0" "[method]udp-socket.set-receive-buffer-size" (func $set_rbuf (param i32 i64 i32)))
  (import "wasi:sockets/udp@0.2.0" "[method]udp-socket.send-buffer-size" (func $sbuf (param i32 i32)))
  (import "wasi:sockets/udp@0.2.0" "[method]udp-socket.set-send-buffer-size" (func $set_sbuf (param i32 i64 i32)))
  (import "wasi:sockets/udp@0.2.0" "[method]udp-socket.subscribe" (func $sub_sock (param i32) (result i32)))
  (import "wasi:sockets/udp@0.2.0" "[method]incoming-datagram-stream.receive" (func $receive (param i32 i64 i32)))
  (import "wasi:sockets/udp@0.2.0" "[method]incoming-datagram-stream.subscribe" (func $sub_in (param i32) (result i32)))
  (import "wasi:sockets/udp@0.2.0" "[method]outgoing-datagram-stream.check-send" (func $check_send (param i32 i32)))
  (import "wasi:sockets/udp@0.2.0" "[method]outgoing-datagram-stream.send" (func $send (param i32 i32 i32 i32)))
  (import "wasi:sockets/udp@0.2.0" "[method]outgoing-datagram-stream.subscribe" (func $sub_out (param i32) (result i32)))
  (memory (export "memory") 2)
  (global $brk (mut i32) (i32.const 8192))
  (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32) (local $p i32)
    (local.set $p (i32.and (i32.add (global.get $brk) (i32.sub (local.get 2) (i32.const 1)))
                           (i32.sub (i32.const 0) (local.get 2))))
    (global.set $brk (i32.add (local.get $p) (local.get 3)))
    (local.get $p))
  (global $net (mut i32) (i32.const -1))
  (global $d_in (mut i32) (i32.const -1))
  (global $d_out (mut i32) (i32.const -1))
  (data (i32.const 400) "ping")
  (data (i32.const 410) "pong!")
  (data (i32.const 420) "ok")
  (data (i32.const 430) "xx")

  ;; result<_, error-code> (or result<bool/u8, error-code>) at `at`: 0, or 100 + the error code
  (func $status (param $at i32) (result i32)
    (if (result i32) (i32.load8_u (local.get $at))
      (then (i32.add (i32.const 100) (i32.load8_u (i32.add (local.get $at) (i32.const 1)))))
      (else (i32.const 0))))
  ;; the same where the error is at `at + 4`, or at `at + 8` (after a u64)
  (func $status4 (param $at i32) (result i32)
    (if (result i32) (i32.load8_u (local.get $at))
      (then (i32.add (i32.const 100) (i32.load8_u (i32.add (local.get $at) (i32.const 4)))))
      (else (i32.const 0))))
  (func $status8 (param $at i32) (result i32)
    (if (result i32) (i32.load8_u (local.get $at))
      (then (i32.add (i32.const 100) (i32.load8_u (i32.add (local.get $at) (i32.const 8)))))
      (else (i32.const 0))))

  (func $network (result i32)
    (if (i32.lt_s (global.get $net) (i32.const 0)) (then (global.set $net (call $instance_network))))
    (global.get $net))

  ;; create-udp-socket(family): the handle, or -1 - the error code
  (func (export "create") (param $family i32) (result i32)
    (call $create (local.get $family) (i32.const 100))
    (if (result i32) (i32.load8_u (i32.const 100))
      (then (i32.sub (i32.const -1) (i32.load8_u (i32.const 104))))
      (else (i32.load (i32.const 104)))))
  (func (export "drop_socket") (param i32) (call $drop_sock (local.get 0)))
  (func (export "drop_in") (param i32) (call $drop_in (local.get 0)))
  (func (export "drop_out") (param i32) (call $drop_out (local.get 0)))

  ;; start-bind to a.b.c.d:port (ipv4), then ::1:port
  (func (export "bind") (param $s i32) (param $port i32) (param $a i32) (param $b i32) (param $c i32) (param $d i32) (result i32)
    (call $start_bind (local.get $s) (call $network) (i32.const 0)
      (local.get $port) (local.get $a) (local.get $b) (local.get $c) (local.get $d)
      (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0)
      (i32.const 120))
    (call $status (i32.const 120)))
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

  ;; the ip-socket-address whose variant tag is at `vt`: ipv4 as port | a << 16 | b << 24 |
  ;; c << 32 | d << 40; ipv6 as 1 << 48 | port | last segment << 16
  (func $pk (param $vt i32) (result i64)
    (if (result i64) (i32.load8_u (local.get $vt))
      (then (i64.or (i64.const 0x1000000000000)
              (i64.or (i64.load16_u (i32.add (local.get $vt) (i32.const 4)))
                      (i64.shl (i64.load16_u (i32.add (local.get $vt) (i32.const 26))) (i64.const 16)))))
      (else (i64.or (i64.load16_u (i32.add (local.get $vt) (i32.const 4)))
              (i64.or (i64.shl (i64.load8_u (i32.add (local.get $vt) (i32.const 6))) (i64.const 16))
              (i64.or (i64.shl (i64.load8_u (i32.add (local.get $vt) (i32.const 7))) (i64.const 24))
              (i64.or (i64.shl (i64.load8_u (i32.add (local.get $vt) (i32.const 8))) (i64.const 32))
                      (i64.shl (i64.load8_u (i32.add (local.get $vt) (i32.const 9))) (i64.const 40)))))))))
  ;; an address result at 200, as `pk`, or -1 - the error code
  (func $packed (result i64)
    (if (result i64) (i32.load8_u (i32.const 200))
      (then (i64.sub (i64.const -1) (i64.load8_u (i32.const 204))))
      (else (call $pk (i32.const 204)))))
  (func (export "local") (param $s i32) (result i64)
    (call $local (local.get $s) (i32.const 200)) (call $packed))
  (func (export "remote") (param $s i32) (result i64)
    (call $remote (local.get $s) (i32.const 200)) (call $packed))
  (func (export "family") (param i32) (result i32) (call $family (local.get 0)))

  ;; stream(remote-address): `has` is 1 for Some(a.b.c.d:port); the streams are `d_in`
  ;; and `d_out`
  (func (export "stream") (param $s i32) (param $has i32) (param $port i32) (param $a i32) (param $b i32) (param $c i32) (param $d i32) (result i32)
    (call $stream (local.get $s) (local.get $has) (i32.const 0)
      (local.get $port) (local.get $a) (local.get $b) (local.get $c) (local.get $d)
      (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0)
      (i32.const 140))
    (if (i32.eqz (i32.load8_u (i32.const 140)))
      (then (global.set $d_in (i32.load (i32.const 144)))
            (global.set $d_out (i32.load (i32.const 148)))))
    (call $status4 (i32.const 140)))
  ;; stream(Some([::]:5)): the status
  (func (export "stream6_unspecified") (param $s i32) (result i32)
    (call $stream (local.get $s) (i32.const 1) (i32.const 1)
      (i32.const 5) (i32.const 0)
      (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0)
      (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0)
      (i32.const 0)
      (i32.const 140))
    (call $status4 (i32.const 140)))
  (func (export "d_in") (result i32) (global.get $d_in))
  (func (export "d_out") (result i32) (global.get $d_out))

  ;; check-send: the permitted count, or -1 - the error code
  (func (export "check_send") (param $h i32) (result i64)
    (call $check_send (local.get $h) (i32.const 240))
    (if (result i64) (i32.load8_u (i32.const 240))
      (then (i64.sub (i64.const -1) (i64.load8_u (i32.const 248))))
      (else (i64.load (i32.const 248)))))
  ;; send `n` copies of the datagram at `ptr` (`len` bytes), to a.b.c.d:port if `has` is 1
  ;; (else without an address): the count sent, or -1 - the error code
  (func (export "send") (param $h i32) (param $n i32) (param $has i32) (param $port i32) (param $a i32) (param $b i32) (param $c i32) (param $d i32) (param $ptr i32) (param $len i32) (result i64)
    (local $i i32) (local $at i32)
    (loop $fill
      (if (i32.lt_u (local.get $i) (local.get $n))
        (then
          (local.set $at (i32.add (i32.const 1200) (i32.mul (local.get $i) (i32.const 44))))
          (i32.store (local.get $at) (local.get $ptr))
          (i32.store offset=4 (local.get $at) (local.get $len))
          (i32.store8 offset=8 (local.get $at) (local.get $has))
          (i32.store8 offset=12 (local.get $at) (i32.const 0))
          (i32.store16 offset=16 (local.get $at) (local.get $port))
          (i32.store8 offset=18 (local.get $at) (local.get $a))
          (i32.store8 offset=19 (local.get $at) (local.get $b))
          (i32.store8 offset=20 (local.get $at) (local.get $c))
          (i32.store8 offset=21 (local.get $at) (local.get $d))
          (local.set $i (i32.add (local.get $i) (i32.const 1)))
          (br $fill))))
    (call $send (local.get $h) (i32.const 1200) (local.get $n) (i32.const 240))
    (if (result i64) (i32.load8_u (i32.const 240))
      (then (i64.sub (i64.const -1) (i64.load8_u (i32.const 248))))
      (else (i64.load (i32.const 248)))))

  ;; the datagram element at `at`, addressed to a.b.c.d:port
  (func $put (param $at i32) (param $port i32) (param $a i32) (param $b i32) (param $c i32) (param $d i32) (param $ptr i32) (param $len i32)
    (i32.store (local.get $at) (local.get $ptr))
    (i32.store offset=4 (local.get $at) (local.get $len))
    (i32.store8 offset=8 (local.get $at) (i32.const 1))
    (i32.store8 offset=12 (local.get $at) (i32.const 0))
    (i32.store16 offset=16 (local.get $at) (local.get $port))
    (i32.store8 offset=18 (local.get $at) (local.get $a))
    (i32.store8 offset=19 (local.get $at) (local.get $b))
    (i32.store8 offset=20 (local.get $at) (local.get $c))
    (i32.store8 offset=21 (local.get $at) (local.get $d)))
  ;; send two datagrams, each to its own address (port a b c d, data ptr and len): the
  ;; count sent, or -1 - the error code
  (func (export "send2") (param $h i32)
    (param $p1 i32) (param $a1 i32) (param $b1 i32) (param $c1 i32) (param $d1 i32) (param $ptr1 i32) (param $len1 i32)
    (param $p2 i32) (param $a2 i32) (param $b2 i32) (param $c2 i32) (param $d2 i32) (param $ptr2 i32) (param $len2 i32)
    (result i64)
    (call $put (i32.const 1200) (local.get $p1) (local.get $a1) (local.get $b1) (local.get $c1) (local.get $d1) (local.get $ptr1) (local.get $len1))
    (call $put (i32.const 1244) (local.get $p2) (local.get $a2) (local.get $b2) (local.get $c2) (local.get $d2) (local.get $ptr2) (local.get $len2))
    (call $send (local.get $h) (i32.const 1200) (i32.const 2) (i32.const 240))
    (if (result i64) (i32.load8_u (i32.const 240))
      (then (i64.sub (i64.const -1) (i64.load8_u (i32.const 248))))
      (else (i64.load (i32.const 248)))))

  ;; receive: the number of datagrams, or -1 - the error code
  (func (export "receive") (param $h i32) (param $max i64) (result i32)
    (call $receive (local.get $h) (local.get $max) (i32.const 800))
    (if (result i32) (i32.load8_u (i32.const 800))
      (then (i32.sub (i32.const -1) (i32.load8_u (i32.const 804))))
      (else (i32.load (i32.const 808)))))
  (func $elem (param $i i32) (result i32)
    (i32.add (i32.load (i32.const 804)) (i32.mul (local.get $i) (i32.const 40))))
  ;; the length of the i-th datagram of the last `receive`, its j-th byte, its source address
  (func (export "rlen") (param $i i32) (result i32) (i32.load offset=4 (call $elem (local.get $i))))
  (func (export "rbyte") (param $i i32) (param $j i32) (result i32)
    (i32.load8_u (i32.add (i32.load (call $elem (local.get $i))) (local.get $j))))
  (func (export "rsrc") (param $i i32) (result i64)
    (call $pk (i32.add (call $elem (local.get $i)) (i32.const 8))))

  ;; socket options: 0, or 100 + the error code
  (func (export "hop_limit") (param i32) (result i32)
    (call $hop (local.get 0) (i32.const 300)) (call $status (i32.const 300)))
  (func (export "set_hop_limit") (param i32) (param i32) (result i32)
    (call $set_hop (local.get 0) (local.get 1) (i32.const 300)) (call $status (i32.const 300)))
  (func (export "ipv6_only") (param i32) (result i32)
    (call $ipv6_only (local.get 0) (i32.const 300)) (call $status (i32.const 300)))
  (func (export "set_ipv6_only") (param i32) (param i32) (result i32)
    (call $set_ipv6_only (local.get 0) (local.get 1) (i32.const 300)) (call $status (i32.const 300)))
  (func (export "receive_buffer") (param i32) (result i32)
    (call $rbuf (local.get 0) (i32.const 320)) (call $status8 (i32.const 320)))
  (func (export "set_receive_buffer") (param i32) (param i64) (result i32)
    (call $set_rbuf (local.get 0) (local.get 1) (i32.const 320)) (call $status (i32.const 320)))
  (func (export "send_buffer") (param i32) (result i32)
    (call $sbuf (local.get 0) (i32.const 320)) (call $status8 (i32.const 320)))
  (func (export "set_send_buffer") (param i32) (param i64) (result i32)
    (call $set_sbuf (local.get 0) (local.get 1) (i32.const 320)) (call $status (i32.const 320)))

  (func (export "sub_socket") (param i32) (result i32) (call $sub_sock (local.get 0)))
  (func (export "sub_in") (param i32) (result i32) (call $sub_in (local.get 0)))
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
)
"#;

fn run(body: &str) -> std::process::Output {
    run_with("TypedHost::new(Vec::new())", body)
}

/// Runs `body` with `HELPERS` in scope.
fn run_with(host: &str, body: &str) -> std::process::Output {
    run_program(
        &component_from_wit(&[IO, SOCKETS], WORLD, "w"),
        CORE,
        host,
        &format!("{HELPERS}\n{body}"),
    )
}

/// Guest-side helpers: `udp` makes an ipv4 socket bound to 127.0.0.1 on a free port and
/// returns it with its address as `local` packs it (the port is the low 16 bits).
const HELPERS: &str = r#"
#[allow(dead_code)]
fn udp(inst: &mut Instance<Glue<TypedHost>>) -> (i32, i64) {
    let s = inst.create(0);
    assert!(s >= 0, "create: {s}");
    assert_eq!(inst.bind(s, 0, 127, 0, 0, 1), 0);
    assert_eq!(inst.finish_bind(s), 0);
    (s, inst.local(s))
}
#[allow(dead_code)]
fn port(address: i64) -> i32 {
    (address & 0xffff) as i32
}
/// Makes the two streams of `s` (no remote address): the handles of the incoming and the
/// outgoing one.
#[allow(dead_code)]
fn streams(inst: &mut Instance<Glue<TypedHost>>, s: i32) -> (i32, i32) {
    assert_eq!(inst.stream(s, 0, 0, 0, 0, 0, 0), 0);
    (inst.d_in(), inst.d_out())
}
/// The bytes of the i-th datagram of the last `receive`.
#[allow(dead_code)]
fn received(inst: &mut Instance<Glue<TypedHost>>, i: i32) -> Vec<u8> {
    (0..inst.rlen(i)).map(|j| inst.rbyte(i, j) as u8).collect()
}
/// Waits for the stream's pollable, then receives at most `max` datagrams.
#[allow(dead_code)]
fn receive_waiting(inst: &mut Instance<Glue<TypedHost>>, d_in: i32, max: i64) -> i32 {
    let p = inst.sub_in(d_in);
    inst.block(p);
    inst.drop_pollable(p);
    inst.receive(d_in, max)
}
"#;

#[test]
fn a_socket_binds_on_loopback_and_reports_its_address_and_family() {
    assert_succeeded(&run("let (s, local) = udp(&mut inst);
         assert_ne!(port(local), 0);
         assert_eq!(local, i64::from(port(local)) | 127 << 16 | 1 << 40);
         assert_eq!(inst.family(s), 0);
         // an ipv6 socket binds ::1: the address is ipv6 (1 << 48) with last segment 1
         let t = inst.create(1);
         assert_eq!(inst.family(t), 1);
         assert_eq!(inst.bind6(t, 0), 0);
         assert_eq!(inst.finish_bind(t), 0);
         let local = inst.local(t);
         assert_ne!(local & 0xffff, 0);
         assert_eq!(local >> 48, 1);
         assert_eq!((local >> 16) & 0xffff, 1);
         // an ipv4 address does not fit an ipv6 socket (invalid-argument = 3)
         let u = inst.create(1);
         assert_eq!(inst.bind(u, 0, 127, 0, 0, 1), 103);"));
}

#[test]
fn an_address_outside_loopback_is_refused_unless_the_host_allows_the_network() {
    // access-denied is 1; 0.0.0.0 is not loopback
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
         // an unbound socket has no local address and no streams
         assert_eq!(inst.local(s), -10);
         assert_eq!(inst.stream(s, 0, 0, 0, 0, 0, 0), 109);
         assert_eq!(inst.bind(s, 0, 127, 0, 0, 1), 0);
         // a second start-bind while one is in progress
         assert_eq!(inst.bind(s, 0, 127, 0, 0, 1), 109);
         // a bound socket cannot bind again
         assert_eq!(inst.finish_bind(s), 0);
         assert_eq!(inst.bind(s, 0, 127, 0, 0, 1), 109);
         assert_eq!(inst.finish_bind(s), 107);
         // an unconnected socket has no remote address
         assert_eq!(inst.remote(s), -10);
         // an address of the wrong family
         let t = udp(&mut inst).0;
         assert_eq!(inst.stream(t, 1, 5, 127, 0, 0, 1), 0);
         let v = inst.create(1);
         assert_eq!(inst.bind6(v, 0), 0);
         assert_eq!(inst.finish_bind(v), 0);
         assert_eq!(inst.stream(v, 1, 5, 127, 0, 0, 1), 103);"));
}

#[test]
fn two_sockets_exchange_datagrams_both_ways_with_the_source_address() {
    assert_succeeded(&run(
        "let ((a, la), (b, lb)) = (udp(&mut inst), udp(&mut inst));
         assert_ne!(port(la), port(lb));
         let (a_in, a_out) = streams(&mut inst, a);
         let (b_in, b_out) = streams(&mut inst, b);
         assert!(inst.check_send(a_out) > 0);
         // a -> b
         assert_eq!(inst.send(a_out, 1, 1, port(lb), 127, 0, 0, 1, 400, 4), 1);
         assert_eq!(receive_waiting(&mut inst, b_in, 10), 1);
         assert_eq!(inst.rlen(0), 4);
         assert_eq!(received(&mut inst, 0), b\"ping\");
         assert_eq!(inst.rsrc(0), la);
         // b -> a
         assert_eq!(inst.send(b_out, 1, 1, port(la), 127, 0, 0, 1, 410, 5), 1);
         assert_eq!(receive_waiting(&mut inst, a_in, 10), 1);
         assert_eq!(inst.rlen(0), 5);
         assert_eq!(received(&mut inst, 0), b\"pong!\");
         assert_eq!(inst.rsrc(0), lb);
         // several datagrams at once, taken at most `max` at a time (max 0 takes none)
         assert_eq!(inst.send(a_out, 3, 1, port(lb), 127, 0, 0, 1, 420, 2), 3);
         let p = inst.sub_in(b_in);
         inst.block(p);
         assert_eq!(inst.receive(b_in, 0), 0);
         assert_eq!(inst.receive(b_in, 2), 2);
         assert_eq!(inst.receive(b_in, 10), 1);
         assert_eq!(inst.receive(b_in, 10), 0);",
    ));
}

#[test]
fn receive_returns_an_empty_list_until_a_datagram_arrives_and_the_pollable_follows() {
    assert_succeeded(&run(
        "let ((a, la), (b, _)) = (udp(&mut inst), udp(&mut inst));
         let (a_in, _) = streams(&mut inst, a);
         let (_, b_out) = streams(&mut inst, b);
         // nothing pending: an empty list, and only the always-ready pollables are ready
         assert_eq!(inst.receive(a_in, 10), 0);
         let (p_in, p_out) = (inst.sub_in(a_in), inst.sub_out(b_out));
         assert_eq!(inst.poll2(p_in, p_out), 0b10);
         // the socket's own pollable is ready at once
         let p_sock = inst.sub_socket(a);
         assert_eq!(inst.poll2(p_sock, p_in), 0b01);
         // a sender on another thread wakes a guest that is blocked in `poll`
         let target = port(la) as u16;
         let sender = std::thread::spawn(move || {
             std::thread::sleep(std::time::Duration::from_millis(50));
             let s = std::net::UdpSocket::bind((\"127.0.0.1\", 0)).unwrap();
             s.send_to(b\"hi\", (\"127.0.0.1\", target)).unwrap();
         });
         inst.block(p_in);
         assert_eq!(inst.poll2(p_in, p_out), 0b11);
         assert_eq!(inst.receive(a_in, 10), 1);
         assert_eq!(inst.rlen(0), 2);
         sender.join().unwrap();
         // drained: not ready again
         assert_eq!(inst.poll2(p_in, p_out), 0b10);",
    ));
}

#[test]
fn a_connected_stream_takes_datagrams_only_from_its_remote_and_sends_there() {
    assert_succeeded(&run(
        "let ((a, la), (b, lb), (c, lc)) = (udp(&mut inst), udp(&mut inst), udp(&mut inst));
         let (a_in, a_out) = streams(&mut inst, a);
         let (c_in, c_out) = streams(&mut inst, c);
         // b is connected to a
         assert_eq!(inst.remote(b), -10);
         assert_eq!(inst.stream(b, 1, port(la), 127, 0, 0, 1), 0);
         let (b_in, b_out) = (inst.d_in(), inst.d_out());
         assert_eq!(inst.remote(b), la);
         // c sends first, then a: b sees only a's datagram
         assert_eq!(inst.send(c_out, 1, 1, port(lb), 127, 0, 0, 1, 430, 2), 1);
         assert_eq!(inst.send(a_out, 1, 1, port(lb), 127, 0, 0, 1, 420, 2), 1);
         assert_eq!(receive_waiting(&mut inst, b_in, 10), 1);
         assert_eq!(inst.rsrc(0), la);
         assert_eq!(received(&mut inst, 0), b\"ok\");
         assert_eq!(inst.receive(b_in, 10), 0);
         // b sends to a without an address, or with a's own address
         assert_eq!(inst.send(b_out, 1, 0, 0, 0, 0, 0, 0, 400, 4), 1);
         assert_eq!(receive_waiting(&mut inst, a_in, 10), 1);
         assert_eq!(inst.rsrc(0), lb);
         assert_eq!(inst.send(b_out, 1, 1, port(la), 127, 0, 0, 1, 400, 4), 1);
         // a different address is invalid-argument (3)
         assert_eq!(inst.send(b_out, 1, 1, port(lc), 127, 0, 0, 1, 400, 4), -4);
         let _ = (c_in, lc);",
    ));
}

#[test]
fn sending_without_an_address_or_to_a_refused_one_fails() {
    // invalid-argument is 3, access-denied 1; the count sent is -1 - the error
    assert_succeeded(&run("let (a, _) = udp(&mut inst);
         let (_, a_out) = streams(&mut inst, a);
         // an unconnected stream needs an address in each datagram
         assert_eq!(inst.send(a_out, 1, 0, 0, 0, 0, 0, 0, 400, 4), -4);
         // port 0 names no destination
         assert_eq!(inst.send(a_out, 1, 1, 0, 127, 0, 0, 1, 400, 4), -4);
         // outside loopback is refused, and nothing is sent
         assert_eq!(inst.send(a_out, 1, 1, 53, 8, 8, 8, 8, 400, 4), -2);
         // a socket bound outside loopback cannot be made: no stream to send over
         let s = inst.create(0);
         assert_eq!(inst.bind(s, 0, 0, 0, 0, 0), 101);"));
}

#[test]
fn the_socket_options_are_not_supported() {
    // not-supported is 2
    assert_succeeded(&run("let (a, _) = udp(&mut inst);
         assert_eq!(inst.hop_limit(a), 102);
         assert_eq!(inst.set_hop_limit(a, 5), 102);
         assert_eq!(inst.ipv6_only(a), 102);
         assert_eq!(inst.set_ipv6_only(a, 1), 102);
         assert_eq!(inst.receive_buffer(a), 102);
         assert_eq!(inst.set_receive_buffer(a, 4096), 102);
         assert_eq!(inst.send_buffer(a), 102);
         assert_eq!(inst.set_send_buffer(a, 4096), 102);"));
}

#[test]
fn dropped_sockets_and_streams_free_their_handles_for_the_next_ones() {
    assert_succeeded(&run("let (a, _) = udp(&mut inst);
         let (b, _) = udp(&mut inst);
         assert_ne!(a, b);
         inst.drop_socket(a);
         assert_eq!(inst.create(0), a);
         let (b_in, b_out) = streams(&mut inst, b);
         let (c_in, c_out) = streams(&mut inst, b);
         assert_ne!(c_in, b_in);
         assert_ne!(c_out, b_out);
         inst.drop_in(b_in);
         inst.drop_out(b_out);
         assert_eq!(streams(&mut inst, b), (b_in, b_out));
         let _ = (c_in, c_out);"));
}

#[test]
fn a_bad_datagram_after_a_good_one_ends_the_batch_without_an_error() {
    // The bad datagrams: port 0, an address outside loopback (no network), 65508 bytes (too
    // large for ipv4), the unspecified address. As the first of a batch each is the error
    // (invalid-argument 3, access-denied 1, datagram-too-large 17, as -1 - n); after a good
    // one the batch ends with 1 sent, and the receiver gets the good one exactly once.
    for (bad, error) in [
        ("0, 127, 0, 0, 1, 400, 4", -4),
        ("53, 8, 8, 8, 8, 400, 4", -2),
        ("PORT, 127, 0, 0, 1, 400, 65508", -18),
        ("PORT, 0, 0, 0, 0, 400, 4", -4),
    ] {
        let bad = bad.replace("PORT", "port(lb)");
        assert_succeeded(&run(&format!(
            "let ((a, _), (b, lb)) = (udp(&mut inst), udp(&mut inst));
             let (_, a_out) = streams(&mut inst, a);
             let (b_in, _) = streams(&mut inst, b);
             let good = (port(lb), 127, 0, 0, 1, 400, 4);
             assert_eq!(inst.send2(a_out, {bad}, good.0, good.1, good.2, good.3, good.4, good.5, good.6), {error});
             assert_eq!(inst.send2(a_out, good.0, good.1, good.2, good.3, good.4, good.5, good.6, {bad}), 1);
             assert_eq!(receive_waiting(&mut inst, b_in, 10), 1);
             assert_eq!(received(&mut inst, 0), b\"ping\");
             assert_eq!(inst.receive(b_in, 10), 0);"
        )));
    }
}

#[test]
fn an_unspecified_or_port_zero_remote_is_invalid_argument_with_or_without_the_network() {
    // as -1 - 3 for send, 103 for stream; the family of the address is checked first
    for host in [
        "TypedHost::new(Vec::new())",
        "TypedHost::new(Vec::new()).with_external_network()",
    ] {
        assert_succeeded(&run_with(
            host,
            "let (a, _) = udp(&mut inst);
             assert_eq!(inst.stream(a, 1, 5, 0, 0, 0, 0), 103);
             assert_eq!(inst.stream(a, 1, 0, 127, 0, 0, 1), 103);
             let (_, a_out) = streams(&mut inst, a);
             assert_eq!(inst.send(a_out, 1, 1, 5, 0, 0, 0, 0, 400, 4), -4);
             assert_eq!(inst.send(a_out, 1, 1, 0, 127, 0, 0, 1, 400, 4), -4);
             // the ipv6 unspecified address `::` on an ipv6 socket
             let v = inst.create(1);
             assert_eq!(inst.bind6(v, 0), 0);
             assert_eq!(inst.finish_bind(v), 0);
             assert_eq!(inst.stream6_unspecified(v), 103);",
        ));
    }
}

#[test]
fn a_replaced_stream_pair_traps_and_takes_no_datagrams() {
    // `stream` called again: only the last pair works. The old incoming stream's pollable is
    // ready at once, and its `receive` and `check-send` trap, as does the old `send`.
    for call in [
        "inst.receive(in1, 10)",
        "inst.check_send(out1)",
        "inst.send(out1, 1, 1, 5, 127, 0, 0, 1, 400, 4)",
    ] {
        let out = run(&format!(
            "let ((a, la), (c, _)) = (udp(&mut inst), udp(&mut inst));
             assert_eq!(inst.stream(a, 1, 5, 127, 0, 0, 1), 0);
             let (in1, out1) = (inst.d_in(), inst.d_out());
             let (in2, out2) = streams(&mut inst, a);
             let (_, c_out) = streams(&mut inst, c);
             let p_old = inst.sub_in(in1);
             let p_new = inst.sub_in(in2);
             assert_eq!(inst.poll2(p_old, p_new), 0b01);
             // a datagram from a third socket: the new stream gets it, the old one did not
             // take it, though it is not from its peer (127.0.0.1:5)
             assert_eq!(inst.send(c_out, 1, 1, port(la), 127, 0, 0, 1, 400, 4), 1);
             assert_eq!(receive_waiting(&mut inst, in2, 10), 1);
             assert_eq!(received(&mut inst, 0), b\"ping\");
             let _ = (out1, out2, in1);
             {call};"
        ));
        assert!(!out.status.success(), "{call} did not trap");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("stale datagram stream"),
            "{call}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
