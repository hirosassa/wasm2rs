//! Arguments, environment and `poll` results on the typed host, with the imports declared
//! in WIT text. The guest hands what it was given back through exports, so the test
//! asserts on plain values.

use crate::wasip2_typed_host::{assert_succeeded, component_from_wit, run_program};

const IO: &str = r#"
package wasi:io@0.2.0;
interface poll {
  resource pollable;
  poll: func(in: list<borrow<pollable>>) -> list<u32>;
}
"#;

const CLI: &str = r#"
package wasi:cli@0.2.0;
interface environment {
  get-environment: func() -> list<tuple<string, string>>;
  get-arguments: func() -> list<string>;
  initial-cwd: func() -> option<string>;
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
  import wasi:cli/environment@0.2.0;
  import wasi:clocks/monotonic-clock@0.2.0;
  import wasi:io/poll@0.2.0;
}
"#;

const CORE: &str = r#"
(module
  (import "wasi:cli/environment@0.2.0" "get-arguments" (func $args (param i32)))
  (import "wasi:cli/environment@0.2.0" "get-environment" (func $env (param i32)))
  (import "wasi:cli/environment@0.2.0" "initial-cwd" (func $cwd (param i32)))
  (import "wasi:io/poll@0.2.0" "[resource-drop]pollable" (func (param i32)))
  (import "wasi:io/poll@0.2.0" "poll" (func $poll (param i32 i32 i32)))
  (import "wasi:clocks/monotonic-clock@0.2.0" "subscribe-duration" (func $dur (param i64) (result i32)))
  (memory (export "memory") 1)
  (global $brk (mut i32) (i32.const 1024))
  (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32) (local $p i32)
    (local.set $p (i32.and (i32.add (global.get $brk) (i32.sub (local.get 2) (i32.const 1)))
                           (i32.sub (i32.const 0) (local.get 2))))
    (global.set $brk (i32.add (local.get $p) (local.get 3)))
    (local.get $p))
  ;; list<string> at 72: (ptr, len), elements are (ptr, len) pairs
  (func (export "argc") (result i32) (call $args (i32.const 72)) (i32.load (i32.const 76)))
  (func (export "arg_len") (param $i i32) (result i32)
    (i32.load offset=4 (i32.add (i32.load (i32.const 72)) (i32.shl (local.get $i) (i32.const 3)))))
  (func (export "arg_byte") (param $i i32) (param $j i32) (result i32)
    (i32.load8_u (i32.add
      (i32.load (i32.add (i32.load (i32.const 72)) (i32.shl (local.get $i) (i32.const 3))))
      (local.get $j))))
  ;; option<string> at 120: tag (u8) at 120, (ptr, len) at 124
  (func (export "cwd_some") (result i32) (call $cwd (i32.const 120)) (i32.load8_u (i32.const 120)))
  (func (export "cwd_len") (result i32) (call $cwd (i32.const 120)) (i32.load (i32.const 128)))
  (func (export "cwd_byte") (param $j i32) (result i32)
    (call $cwd (i32.const 120))
    (i32.load8_u (i32.add (i32.load (i32.const 124)) (local.get $j))))
  ;; list<tuple<string, string>> at 80: 16-byte elements (key, value)
  (func (export "envc") (result i32) (call $env (i32.const 80)) (i32.load (i32.const 84)))
  (func (export "key_len") (param $i i32) (result i32)
    (i32.load offset=4 (i32.add (i32.load (i32.const 80)) (i32.shl (local.get $i) (i32.const 4)))))
  (func (export "key_byte") (param $i i32) (param $j i32) (result i32)
    (i32.load8_u (i32.add
      (i32.load (i32.add (i32.load (i32.const 80)) (i32.shl (local.get $i) (i32.const 4))))
      (local.get $j))))
  (func (export "val_len") (param $i i32) (result i32)
    (i32.load offset=12 (i32.add (i32.load (i32.const 80)) (i32.shl (local.get $i) (i32.const 4)))))
  (func (export "val_byte") (param $i i32) (param $j i32) (result i32)
    (i32.load8_u (i32.add
      (i32.load offset=8 (i32.add (i32.load (i32.const 80)) (i32.shl (local.get $i) (i32.const 4))))
      (local.get $j))))
  ;; two timers that are already due: `poll` reports both, in order; returns the count
  ;; and leaves the indices in memory (list<u32> at 104)
  (func (export "poll_two") (result i32)
    (i32.store (i32.const 96) (call $dur (i64.const 0)))
    (i32.store (i32.const 100) (call $dur (i64.const 0)))
    (call $poll (i32.const 96) (i32.const 2) (i32.const 104))
    (i32.load (i32.const 108)))
  (func (export "poll_index") (param $i i32) (result i32)
    (i32.load (i32.add (i32.load (i32.const 104)) (i32.shl (local.get $i) (i32.const 2))))))
"#;

fn run(host: &str, body: &str) -> std::process::Output {
    run_program(
        &component_from_wit(&[IO, CLI, CLOCKS], WORLD, "w"),
        CORE,
        host,
        body,
    )
}

/// Reads the `i`-th string of a list the guest exposes through `len` and `byte`.
const READERS: &str = r#"
#[allow(unused_variables)]
let arg = |inst: &mut Instance<Glue<TypedHost>>, i: i32| -> String {
    (0..inst.arg_len(i)).map(|j| inst.arg_byte(i, j) as u8 as char).collect()
};
#[allow(unused_variables)]
let key = |inst: &mut Instance<Glue<TypedHost>>, i: i32| -> String {
    (0..inst.key_len(i)).map(|j| inst.key_byte(i, j) as u8 as char).collect()
};
#[allow(unused_variables)]
let val = |inst: &mut Instance<Glue<TypedHost>>, i: i32| -> String {
    (0..inst.val_len(i)).map(|j| inst.val_byte(i, j) as u8 as char).collect()
};
"#;

#[test]
fn the_guest_sees_the_arguments_the_host_was_given() {
    let out = run(
        r#"TypedHost::new(Vec::new()).with_arguments(vec!["prog".to_string(), "x".to_string()])"#,
        &format!(
            "{READERS} assert_eq!(inst.argc(), 2); \
             assert_eq!(arg(&mut inst, 0), \"prog\"); assert_eq!(arg(&mut inst, 1), \"x\");"
        ),
    );
    assert_succeeded(&out);
}

#[test]
fn a_host_without_arguments_gives_an_empty_list() {
    assert_succeeded(&run(
        "TypedHost::new(Vec::new())",
        "assert_eq!(inst.argc(), 0);",
    ));
}

#[test]
fn the_guest_sees_the_environment_the_host_was_given() {
    let out = run(
        r#"TypedHost::new(vec![("K".to_string(), "VX".to_string())])"#,
        &format!(
            "{READERS} assert_eq!(inst.envc(), 1); \
             assert_eq!(key(&mut inst, 0), \"K\"); assert_eq!(val(&mut inst, 0), \"VX\");"
        ),
    );
    assert_succeeded(&out);
}

#[test]
fn poll_returns_the_indices_of_all_ready_pollables() {
    assert_succeeded(&run(
        "TypedHost::new(Vec::new())",
        "assert_eq!(inst.poll_two(), 2); assert_eq!(inst.poll_index(0), 0); \
         assert_eq!(inst.poll_index(1), 1);",
    ));
}

#[test]
fn a_host_without_a_preopen_has_no_initial_cwd() {
    assert_succeeded(&run(
        "TypedHost::new(Vec::new())",
        "assert_eq!(inst.cwd_some(), 0);",
    ));
}

#[test]
fn the_initial_cwd_is_the_root_of_the_preopen() {
    // the preopen is shown as "/", so that is where the guest starts
    let dir = std::env::temp_dir();
    let out = run(
        &format!(
            "TypedHost::new(Vec::new()).with_preopen(std::path::PathBuf::from({:?}))",
            dir
        ),
        "assert_eq!(inst.cwd_some(), 1); assert_eq!(inst.cwd_len(), 1); \
         assert_eq!(inst.cwd_byte(0), b'/' as i32);",
    );
    assert_succeeded(&out);
}
