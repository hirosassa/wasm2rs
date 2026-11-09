//! `wasi:sockets` ip-name-lookup on the typed host, with the imports declared in WIT text.
//! Only `localhost` and numeric addresses resolve without `with_external_network`, so the
//! tests never touch a real resolver.

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
  type ipv4-address = tuple<u8, u8, u8, u8>;
  type ipv6-address = tuple<u16, u16, u16, u16, u16, u16, u16, u16>;
  variant ip-address { ipv4(ipv4-address), ipv6(ipv6-address) }
}
interface instance-network {
  use network.{network};
  instance-network: func() -> network;
}
interface ip-name-lookup {
  use wasi:io/poll@0.2.0.{pollable};
  use network.{network, error-code, ip-address};
  resource resolve-address-stream {
    resolve-next-address: func() -> result<option<ip-address>, error-code>;
    subscribe: func() -> pollable;
  }
  resolve-addresses: func(network: borrow<network>, name: string) -> result<resolve-address-stream, error-code>;
}
"#;

const WORLD: &str = r#"
package test:t;
world w {
  import wasi:io/poll@0.2.0;
  import wasi:sockets/network@0.2.0;
  import wasi:sockets/instance-network@0.2.0;
  import wasi:sockets/ip-name-lookup@0.2.0;
}
"#;

/// The names the guest can resolve: name `k` is in memory at `1000 + 64 * k`.
const NAMES: [&str; 10] = [
    "localhost",
    "127.0.0.1",
    "::1",
    "LocalHost.",
    "",
    "bad name",
    "exa$mple.com",
    "256.1.1.1",
    "example.test",
    "2001:db8::1",
];

fn core() -> String {
    let data: String = NAMES
        .iter()
        .enumerate()
        .map(|(k, name)| format!("(data (i32.const {}) \"{name}\")\n", 1000 + 64 * k))
        .collect();
    CORE.replace(";; NAMES", &data)
}

const CORE: &str = r#"
(module
  (import "wasi:io/poll@0.2.0" "[resource-drop]pollable" (func $drop_pollable (param i32)))
  (import "wasi:io/poll@0.2.0" "[method]pollable.block" (func $block (param i32)))
  (import "wasi:io/poll@0.2.0" "poll" (func $poll (param i32 i32 i32)))
  (import "wasi:sockets/network@0.2.0" "[resource-drop]network" (func $drop_net (param i32)))
  (import "wasi:sockets/instance-network@0.2.0" "instance-network" (func $instance_network (result i32)))
  (import "wasi:sockets/ip-name-lookup@0.2.0" "resolve-addresses" (func $resolve (param i32 i32 i32 i32)))
  (import "wasi:sockets/ip-name-lookup@0.2.0" "[resource-drop]resolve-address-stream" (func $drop_stream (param i32)))
  (import "wasi:sockets/ip-name-lookup@0.2.0" "[method]resolve-address-stream.resolve-next-address" (func $next (param i32 i32)))
  (import "wasi:sockets/ip-name-lookup@0.2.0" "[method]resolve-address-stream.subscribe" (func $sub (param i32) (result i32)))
  (memory (export "memory") 1)
  (global $brk (mut i32) (i32.const 8192))
  (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32) (local $p i32)
    (local.set $p (i32.and (i32.add (global.get $brk) (i32.sub (local.get 2) (i32.const 1)))
                           (i32.sub (i32.const 0) (local.get 2))))
    (global.set $brk (i32.add (local.get $p) (local.get 3)))
    (local.get $p))
  (global $net (mut i32) (i32.const -1))
  ;; NAMES

  (func $network (result i32)
    (if (i32.lt_s (global.get $net) (i32.const 0)) (then (global.set $net (call $instance_network))))
    (global.get $net))

  ;; resolve-addresses(name at ptr, len bytes): the stream, or -1 - the error code
  (func (export "resolve") (param $ptr i32) (param $len i32) (result i32)
    (call $resolve (call $network) (local.get $ptr) (local.get $len) (i32.const 100))
    (if (result i32) (i32.load8_u (i32.const 100))
      (then (i32.sub (i32.const -1) (i32.load8_u (i32.const 104))))
      (else (i32.load (i32.const 104)))))
  (func (export "drop_stream") (param i32) (call $drop_stream (local.get 0)))

  ;; resolve-next-address: -1 - the error code; 0 for none; an ipv4 address as
  ;; 1 << 40 | a << 24 | b << 16 | c << 8 | d; an ipv6 one as 2 << 40 | first segment << 16 | last
  ;; (the result is at 300: tag and error at 302, option tag at 302, address tag at 304,
  ;; its bytes from 306)
  (func (export "next") (param $s i32) (result i64)
    (call $next (local.get $s) (i32.const 300))
    (if (result i64) (i32.load8_u (i32.const 300))
      (then (i64.sub (i64.const -1) (i64.load8_u (i32.const 302))))
      (else
        (if (result i64) (i32.eqz (i32.load8_u (i32.const 302)))
          (then (i64.const 0))
          (else
            (if (result i64) (i32.load8_u (i32.const 304))
              (then (i64.or (i64.const 0x20000000000)
                      (i64.or (i64.shl (i64.load16_u (i32.const 306)) (i64.const 16))
                              (i64.load16_u (i32.const 320)))))
              (else (i64.or (i64.const 0x10000000000)
                      (i64.or (i64.shl (i64.load8_u (i32.const 306)) (i64.const 24))
                      (i64.or (i64.shl (i64.load8_u (i32.const 307)) (i64.const 16))
                      (i64.or (i64.shl (i64.load8_u (i32.const 308)) (i64.const 8))
                              (i64.load8_u (i32.const 309)))))))))))))
  (func (export "sub") (param i32) (result i32) (call $sub (local.get 0)))
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
    run_with("WasiHost::new(Vec::new())", body)
}

/// Runs `body` with `HELPERS` in scope.
fn run_with(host: &str, body: &str) -> std::process::Output {
    run_program(
        &component_from_wit(&[IO, SOCKETS], WORLD, "w"),
        &core(),
        host,
        &format!("{HELPERS}\n{body}"),
    )
}

/// A guest-side expression resolving `NAMES[k]`.
fn resolve(k: usize) -> String {
    format!("inst.resolve({}, {})", 1000 + 64 * k, NAMES[k].len())
}

/// Guest-side helper: every address a stream yields, up to its end (`none`).
const HELPERS: &str = r#"
#[allow(dead_code)]
fn addresses(inst: &mut Instance<Glue<WasiHost>>, s: i32) -> Vec<i64> {
    let mut all = Vec::new();
    loop {
        let next = inst.next(s);
        assert!(next >= 0, "resolve-next-address: {next}");
        if next == 0 {
            return all;
        }
        all.push(next);
    }
}
"#;

const V4_LOOPBACK: i64 = 1 << 40 | 127 << 24 | 1;
const V6_LOOPBACK: i64 = 2 << 40 | 1;

#[test]
fn localhost_resolves_to_both_loopback_addresses_in_any_case() {
    for k in [0, 3] {
        assert_succeeded(&run(&format!(
            "let s = {};
             assert!(s >= 0, \"{{s}}\");
             assert_eq!(addresses(&mut inst, s), [{V4_LOOPBACK}, {V6_LOOPBACK}]);",
            resolve(k)
        )));
    }
}

#[test]
fn numeric_addresses_resolve_to_themselves() {
    assert_succeeded(&run(&format!(
        "let a = {};
         assert_eq!(addresses(&mut inst, a), [{V4_LOOPBACK}]);
         let b = {};
         assert_eq!(addresses(&mut inst, b), [{V6_LOOPBACK}]);
         let c = {};
         assert_eq!(addresses(&mut inst, c), [2 << 40 | 0x2001 << 16 | 1]);",
        resolve(1),
        resolve(2),
        resolve(9)
    )));
}

#[test]
fn the_stream_ends_with_none_and_its_pollable_is_ready() {
    assert_succeeded(&run(&format!(
        "let s = {};
         let p = inst.sub(s);
         // ready before any address is read, and after the end
         assert_eq!(inst.poll2(p, p), 0b11);
         assert_eq!(inst.next(s), {V4_LOOPBACK});
         assert_eq!(inst.next(s), {V6_LOOPBACK});
         assert_eq!(inst.next(s), 0);
         assert_eq!(inst.next(s), 0);
         inst.block(p);
         assert_eq!(inst.poll2(p, p), 0b11);",
        resolve(0)
    )));
}

#[test]
fn a_name_that_is_not_a_host_name_is_invalid_argument() {
    // invalid-argument is 3, as -1 - 3
    for k in [4, 5, 6, 7] {
        assert_succeeded(&run(&format!("assert_eq!({}, -4);", resolve(k))));
    }
}

#[test]
fn another_name_is_refused_unless_the_host_allows_the_network() {
    // permanent-resolver-failure is 20, as -1 - 20; no resolver is asked
    assert_succeeded(&run(&format!("assert_eq!({}, -21);", resolve(8))));
}

#[test]
fn a_host_with_the_network_still_resolves_localhost_without_a_resolver() {
    assert_succeeded(&run_with(
        "WasiHost::new(Vec::new()).with_external_network()",
        &format!(
            "let s = {};
             assert_eq!(addresses(&mut inst, s), [{V4_LOOPBACK}, {V6_LOOPBACK}]);
             // a syntactically invalid name never reaches the resolver
             assert_eq!({}, -4);",
            resolve(0),
            resolve(5)
        ),
    ));
}

#[test]
fn a_dropped_stream_frees_its_handle_for_the_next_one() {
    assert_succeeded(&run(&format!(
        "let a = {};
         let b = {};
         assert_ne!(a, b);
         inst.drop_stream(a);
         assert_eq!({}, a);",
        resolve(0),
        resolve(1),
        resolve(2)
    )));
}
