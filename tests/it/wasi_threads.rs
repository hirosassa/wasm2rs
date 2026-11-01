//! `wasi::thread-spawn`: the generated module starts a thread that runs a fresh
//! instance over the same shared memory and calls its exported
//! `wasi_thread_start(tid, start_arg)`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code"
)]

use crate::common;

use common::{compile_run, compile_run_raw, expect_trap_with};
use wasm2rs::{TranspileError, transpile};

const SPAWN_IMPORT: &str = r#"
      (import "env" "memory" (memory 1 1 shared))
      (import "wasi" "thread-spawn" (func $spawn (param i32) (result i32)))"#;

#[test]
fn a_spawned_thread_runs_wasi_thread_start_on_the_shared_memory() {
    // The child stores `start_arg * 2` at address 0 and wakes the parent, which
    // is parked on that address.
    compile_run(
        "wasi_threads_single",
        &format!(
            r#"(module {SPAWN_IMPORT}
              (func (export "wasi_thread_start") (param i32 i32)
                i32.const 0
                local.get 1
                i32.const 2
                i32.mul
                i32.atomic.store
                i32.const 0
                i32.const 1
                memory.atomic.notify
                drop)
              (func (export "run") (result i32)
                i32.const 21
                call $spawn
                i32.const 0
                i32.le_s
                if unreachable end
                i32.const 0
                i32.const 0
                i64.const 5000000000
                memory.atomic.wait32
                drop
                i32.const 0
                i32.atomic.load))"#
        ),
        "let mut inst = Instance::new(SharedMemory::new(65536)); assert_eq!(inst.func2(), 42);",
    );
}

#[test]
fn thread_spawn_on_a_non_shared_memory_is_rejected() {
    let wasm = wat::parse_str(
        r#"(module
            (import "wasi" "thread-spawn" (func (param i32) (result i32)))
            (memory 1)
            (func (export "wasi_thread_start") (param i32 i32)))"#,
    )
    .unwrap();
    assert!(matches!(
        transpile(&wasm),
        Err(TranspileError::Unsupported(m)) if m.contains("thread-spawn")
    ));
}

#[test]
fn thread_spawn_without_a_wasi_thread_start_export_is_rejected() {
    let wasm = wat::parse_str(
        r#"(module
            (import "env" "memory" (memory 1 1 shared))
            (import "wasi" "thread-spawn" (func (param i32) (result i32))))"#,
    )
    .unwrap();
    assert!(matches!(
        transpile(&wasm),
        Err(TranspileError::Unsupported(m)) if m.contains("wasi_thread_start")
    ));
}

#[test]
fn a_trap_in_a_spawned_thread_terminates_the_whole_process() {
    // The child traps at once while the parent is parked for up to 5s. If the
    // trap only ended the child, the parent would time out and exit 0.
    expect_trap_with(
        "wasi_threads_trap",
        &format!(
            r#"(module {SPAWN_IMPORT}
              (func (export "wasi_thread_start") (param i32 i32)
                unreachable)
              (func (export "run") (result i32)
                i32.const 0
                call $spawn
                drop
                i32.const 0
                i32.const 0
                i64.const 5000000000
                memory.atomic.wait32))"#
        ),
        "let mut inst = Instance::new(SharedMemory::new(65536)); inst.func2();",
        "thread 1 trapped",
    );
}

#[test]
fn several_threads_get_distinct_tids_and_share_the_memory() {
    // Four children each add their tid to address 0, then bump a done counter at
    // address 4 and wake the parent. Distinct tids 1..=4 sum to 10.
    compile_run(
        "wasi_threads_many",
        &format!(
            r#"(module {SPAWN_IMPORT}
              (func (export "wasi_thread_start") (param i32 i32)
                i32.const 0
                local.get 0
                i32.atomic.rmw.add
                drop
                i32.const 4
                i32.const 1
                i32.atomic.rmw.add
                drop
                i32.const 4
                i32.const 1
                memory.atomic.notify
                drop)
              (func $spawn_one
                i32.const 0
                call $spawn
                i32.const 0
                i32.le_s
                if unreachable end)
              (func (export "run") (result i32)
                call $spawn_one
                call $spawn_one
                call $spawn_one
                call $spawn_one
                (loop $again
                  i32.const 4
                  i32.atomic.load
                  i32.const 4
                  i32.ne
                  if
                    i32.const 4
                    i32.const 0
                    i64.const 5000000000
                    memory.atomic.wait32
                    drop
                    br $again
                  end)
                i32.const 0
                i32.atomic.load))"#
        ),
        "let mut inst = Instance::new(SharedMemory::new(65536)); assert_eq!(inst.func3(), 10);",
    );
}

#[test]
fn thread_spawn_works_alongside_a_host_import() {
    // With a host import the instance is generic over `H`, which each spawned
    // thread receives as a clone. The child calls the host from its own thread.
    compile_run_raw(
        "wasi_threads_with_host",
        &format!(
            r#"(module {SPAWN_IMPORT}
              (import "env" "ping" (func $ping (param i32)))
              (func (export "wasi_thread_start") (param i32 i32)
                local.get 1
                call $ping)
              (func (export "run")
                i32.const 7
                call $spawn
                drop))"#
        ),
        r#"
        #[derive(Clone)]
        struct Host(std::sync::mpsc::Sender<i32>);
        impl Imports for Host {
            fn import1(&mut self, a0: i32) { self.0.send(a0).unwrap(); }
        }
        fn main() {
            let (tx, rx) = std::sync::mpsc::channel();
            let mut inst = Instance::new(SharedMemory::new(65536), Host(tx));
            inst.func3();
            assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap(), 7);
        }
        "#,
    );
}
