//! `wasi:random` on the typed host, with the imports declared in WIT text.

use crate::wasip2_typed_host::{assert_succeeded, component_from_wit, run_program};

const RANDOM: &str = r#"
package wasi:random@0.2.0;
interface random {
  get-random-bytes: func(len: u64) -> list<u8>;
  get-random-u64: func() -> u64;
}
interface insecure {
  get-insecure-random-bytes: func(len: u64) -> list<u8>;
  get-insecure-random-u64: func() -> u64;
}
interface insecure-seed {
  insecure-seed: func() -> tuple<u64, u64>;
}
"#;

const WORLD: &str = r#"
package test:t;
world w {
  import wasi:random/random@0.2.0;
  import wasi:random/insecure@0.2.0;
  import wasi:random/insecure-seed@0.2.0;
}
"#;

const CORE: &str = r#"
(module
  (import "wasi:random/random@0.2.0" "get-random-bytes" (func $bytes (param i64 i32)))
  (import "wasi:random/random@0.2.0" "get-random-u64" (func $u64 (result i64)))
  (import "wasi:random/insecure@0.2.0" "get-insecure-random-bytes" (func $ibytes (param i64 i32)))
  (import "wasi:random/insecure@0.2.0" "get-insecure-random-u64" (func $iu64 (result i64)))
  (import "wasi:random/insecure-seed@0.2.0" "insecure-seed" (func $seed (param i32)))
  (memory (export "memory") 1)
  (global $brk (mut i32) (i32.const 1024))
  (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32) (local $p i32)
    (local.set $p (i32.and (i32.add (global.get $brk) (i32.sub (local.get 2) (i32.const 1)))
                           (i32.sub (i32.const 0) (local.get 2))))
    (global.set $brk (i32.add (local.get $p) (local.get 3)))
    (local.get $p))
  ;; the length of the list that `get-random-bytes(16)` returns
  (func (export "count") (result i32)
    (call $bytes (i64.const 16) (i32.const 100))
    (i32.load (i32.const 104)))
  ;; the length of the list that `get-insecure-random-bytes(5)` returns
  (func (export "icount") (result i32)
    (call $ibytes (i64.const 5) (i32.const 100))
    (i32.load (i32.const 104)))
  (func (export "inext") (result i64) (call $iu64))
  ;; get-random-bytes with a length of 1 GiB
  (func (export "huge") (call $bytes (i64.const 0x40000000) (i32.const 100)))
  ;; the two halves of the seed differ (tuple<u64, u64> at 104, 8-aligned)
  (func (export "seed") (result i32)
    (call $seed (i32.const 104))
    (i64.ne (i64.load (i32.const 104)) (i64.load (i32.const 112))))
  (func (export "next") (result i64) (call $u64)))
"#;

fn run(body: &str) -> std::process::Output {
    run_program(
        &component_from_wit(&[RANDOM], WORLD, "w"),
        CORE,
        "TypedHost::new(Vec::new())",
        body,
    )
}

#[test]
fn get_random_bytes_returns_the_requested_number_of_bytes() {
    assert_succeeded(&run("assert_eq!(inst.count(), 16);"));
}

#[test]
fn get_random_u64_gives_a_different_value_each_call() {
    assert_succeeded(&run(
        "let a = inst.next(); let b = inst.next(); assert_ne!(a, b);",
    ));
}

#[test]
fn get_insecure_random_bytes_returns_the_requested_number_of_bytes() {
    assert_succeeded(&run("assert_eq!(inst.icount(), 5);"));
}

#[test]
fn get_insecure_random_u64_gives_a_different_value_each_call() {
    assert_succeeded(&run(
        "let a = inst.inext(); let b = inst.inext(); assert_ne!(a, b);",
    ));
}

#[test]
fn insecure_seed_gives_two_values() {
    assert_succeeded(&run("assert_eq!(inst.seed(), 1);"));
}

#[test]
fn a_guest_chosen_length_over_64_mib_traps() {
    let out = run("inst.huge();");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("length over 64 MiB"), "{stderr}");
}
