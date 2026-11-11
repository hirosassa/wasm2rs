//! `env.__pthread_create_js`: emscripten's pthread entry. The generated module
//! starts a thread that runs a fresh instance over the same shared memory and
//! drives emscripten's own start-up sequence: `_emscripten_thread_init`,
//! `_emscripten_tls_init`, the start routine from the table, then
//! `_emscripten_thread_exit`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code"
)]

use crate::common;

use common::compile_run;
use wasm2rs::{TranspileError, transpile};

#[test]
fn a_created_pthread_runs_the_emscripten_start_up_sequence() {
    // The child records each step in order at addresses 0, 4 and 8 (init with
    // its pthread pointer, tls init, then the start routine with its argument
    // plus one) and finally `_emscripten_thread_exit` bumps address 12 and
    // wakes the parent parked on it.
    compile_run(
        "emscripten_pthread_sequence",
        r#"(module
            (import "env" "memory" (memory 1 1 shared))
            (import "env" "__pthread_create_js"
              (func $create (param i32 i32 i32 i32) (result i32)))
            (table 2 funcref)
            (elem (i32.const 1) $start)
            (func (export "_emscripten_thread_init")
              (param i32 i32 i32 i32 i32 i32)
              i32.const 0
              local.get 0
              i32.atomic.store)
            (func (export "_emscripten_tls_init") (result i32)
              i32.const 4
              i32.const 7
              i32.atomic.store
              i32.const 0)
            (func $start (param i32) (result i32)
              i32.const 8
              local.get 0
              i32.const 1
              i32.add
              i32.atomic.store
              i32.const 0)
            (func (export "_emscripten_thread_exit") (param i32)
              i32.const 12
              i32.const 1
              i32.atomic.store
              i32.const 12
              i32.const 1
              memory.atomic.notify
              drop)
            (func (export "run") (result i32)
              i32.const 100
              i32.const 0
              i32.const 1
              i32.const 41
              call $create
              if unreachable end
              i32.const 12
              i32.const 0
              i64.const 5000000000
              memory.atomic.wait32
              drop
              i32.const 12 i32.atomic.load
              i32.const 8 i32.atomic.load
              i32.const 4 i32.atomic.load
              i32.const 0 i32.atomic.load
              i32.add i32.add i32.add))"#,
        "let mut inst = Instance::new(SharedMemory::new(65536)); assert_eq!(inst.func5(), 1 + 42 + 7 + 100);",
    );
}

const CREATE: &str = r#"(import "env" "__pthread_create_js"
              (func (param i32 i32 i32 i32) (result i32)))"#;

fn rejection(wat: &str) -> String {
    let wasm = wat::parse_str(wat).unwrap();
    match transpile(&wasm).as_ref().map_err(TranspileError::root) {
        Err(TranspileError::Unsupported(m)) => m.clone(),
        other => panic!("expected Unsupported, got {:?}", other.map(|_| ())),
    }
}

#[test]
fn pthread_create_on_a_non_shared_memory_is_rejected() {
    let m = rejection(&format!("(module {CREATE} (memory 1) (table 1 funcref))"));
    assert!(
        m.contains("__pthread_create_js") && m.contains("shared memory"),
        "{m}"
    );
}

#[test]
fn pthread_create_without_a_module_table_is_rejected() {
    let m = rejection(&format!(
        r#"(module (import "env" "memory" (memory 1 1 shared)) {CREATE})"#
    ));
    assert!(
        m.contains("__pthread_create_js") && m.contains("table"),
        "{m}"
    );
}

#[test]
fn pthread_create_without_the_thread_start_up_exports_is_rejected() {
    let m = rejection(&format!(
        r#"(module (import "env" "memory" (memory 1 1 shared)) {CREATE} (table 1 funcref))"#
    ));
    assert!(m.contains("_emscripten_thread_init"), "{m}");
}

#[test]
fn a_thread_start_up_export_with_the_wrong_type_is_rejected() {
    let m = rejection(&format!(
        r#"(module (import "env" "memory" (memory 1 1 shared)) {CREATE} (table 1 funcref)
            (func (export "_emscripten_thread_init") (param i32))
            (func (export "_emscripten_tls_init") (result i32) i32.const 0)
            (func (export "_emscripten_thread_exit") (param i32)))"#
    ));
    assert!(
        m.contains("_emscripten_thread_init") && m.contains("must have type"),
        "{m}"
    );
}

#[test]
fn emscripten_bookkeeping_imports_are_native_no_ops_needing_no_host() {
    // None of these is routed to a host trait: the module is built with no
    // host and calls all six.
    compile_run(
        "emscripten_noops",
        r#"(module
            (import "env" "emscripten_check_blocking_allowed" (func $a))
            (import "env" "emscripten_exit_with_live_runtime" (func $g))
            (import "env" "_emscripten_thread_cleanup" (func $b (param i32)))
            (import "env" "_emscripten_thread_set_strongref" (func $c (param i32)))
            (import "env" "_emscripten_thread_mailbox_await" (func $d (param i32)))
            (import "env" "_emscripten_notify_mailbox_postmessage"
              (func $f (param i32 i32)))
            (func (export "run") (result i32)
              call $a
              call $g
              i32.const 1 call $b
              i32.const 1 call $c
              i32.const 1 call $d
              i32.const 1 i32.const 2 call $f
              i32.const 7))"#,
        "let mut inst = Instance::new(); assert_eq!(inst.func6(), 7);",
    );
}

#[test]
fn the_main_thread_init_import_runs_thread_init_and_tls_init_as_the_main_thread() {
    // Emscripten's wasm startup hands its own pthread pointer to the host,
    // which initialises the main thread: `is_main = 1`, `is_runtime = 1`.
    compile_run(
        "emscripten_main_thread_init",
        r#"(module
            (import "env" "memory" (memory 1 1 shared))
            (import "env" "_emscripten_init_main_thread_js" (func $init (param i32)))
            (func (export "_emscripten_thread_init")
              (param i32 i32 i32 i32 i32 i32)
              i32.const 0 local.get 0 i32.store
              i32.const 4 local.get 1 i32.store
              i32.const 8 local.get 2 i32.store)
            (func (export "_emscripten_tls_init") (result i32)
              i32.const 12 i32.const 5 i32.store
              i32.const 0)
            (func (export "_emscripten_thread_exit") (param i32))
            (func (export "run") (result i32)
              i32.const 9 call $init
              i32.const 0 i32.load
              i32.const 4 i32.load
              i32.const 8 i32.load
              i32.const 12 i32.load
              i32.add i32.add i32.add))"#,
        "let mut inst = Instance::new(SharedMemory::new(65536)); assert_eq!(inst.func4(), 9 + 1 + 1 + 5);",
    );
}

#[test]
fn emscripten_get_now_is_a_native_millisecond_clock_that_never_goes_back() {
    // No host is given: the clock is generated natively. Two reads around a
    // 20 ms sleep must differ by at least that much, in milliseconds.
    compile_run(
        "emscripten_get_now",
        r#"(module
            (import "env" "emscripten_get_now" (func $now (result f64)))
            (func (export "run") (result f64) call $now))"#,
        "let mut inst = Instance::new();
         let a = inst.func1();
         std::thread::sleep(std::time::Duration::from_millis(20));
         let b = inst.func1();
         assert!(a >= 0.0 && b - a >= 20.0 && b - a < 5000.0, \"{a} {b}\");",
    );
}

#[test]
fn emscripten_resize_heap_refuses_to_grow_the_fixed_shared_memory() {
    compile_run(
        "emscripten_resize_heap",
        r#"(module
            (import "env" "memory" (memory 1 1 shared))
            (import "env" "emscripten_resize_heap" (func $grow (param i32) (result i32)))
            (func (export "run") (param i32) (result i32) local.get 0 call $grow))"#,
        "let mut inst = Instance::new(SharedMemory::new(65536)); assert_eq!(inst.func1(1 << 20), 0);",
    );
}

#[test]
fn emscripten_exit_ends_the_process_with_the_given_code() {
    let wat = r#"(module
        (import "env" "exit" (func $exit (param i32)))
        (func (export "run") (param i32) local.get 0 call $exit))"#;
    let generated = transpile(&wat::parse_str(wat).unwrap()).unwrap();
    let program =
        format!("{generated}\nfn main() {{ let mut i = Instance::new(); i.func1(42); }}\n");
    let bin = common::build("emscripten_exit", &program);
    let status = std::process::Command::new(&bin).status().unwrap();
    assert_eq!(status.code(), Some(42));
}
