//! End-to-end tests for *real* multi-threaded atomics over a `shared` memory.
//!
//! When a module's memory is declared `shared`, wasm2rs backs it with a
//! thread-shareable handle (`SharedMemory`, cheaply clonable, `Send + Sync`).
//! Sibling instances built with `Instance::with_memory(handle)` share the same
//! linear memory (each keeps its own globals/tables), so atomic operations issued
//! from different OS threads are genuinely atomic and `wait`/`notify` actually
//! block and wake. Non-shared memory keeps the fast single-threaded path (see
//! tests/atomics.rs), which these tests do not touch.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::arithmetic_side_effects,
    clippy::float_cmp,
    clippy::lossy_float_literal,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::unwrap_in_result,
    reason = "test code"
)]

mod common;

use common::compile_run_raw;

#[test]
fn concurrent_atomic_rmw_add_is_atomic() {
    // Eight threads each do 10_000 `i32.atomic.rmw.add 1` at address 0 over one
    // shared memory. If the RMW were not atomic across threads, updates would be
    // lost and the total would fall short; an atomic RMW yields exactly 80_000.
    compile_run_raw(
        "atomics_threads_rmw",
        r#"(module
            (memory 1 1 shared)
            (func (export "inc")
              i32.const 0 i32.const 1 i32.atomic.rmw.add drop)
            (func (export "get") (result i32)
              i32.const 0 i32.atomic.load))"#,
        r#"fn main() {
            let inst = Instance::new();
            let handle = inst.shared_memory();
            let threads = 8;
            let per = 10_000;
            let mut joins = Vec::new();
            for _ in 0..threads {
                let m = handle.clone();
                joins.push(std::thread::spawn(move || {
                    let mut t = Instance::with_memory(m);
                    for _ in 0..per { t.func0(); }
                }));
            }
            for j in joins { j.join().unwrap(); }
            let mut reader = Instance::with_memory(handle);
            assert_eq!(reader.func1(), threads * per);
        }"#,
    );
}

#[test]
fn wait_returns_one_on_value_mismatch() {
    // A `memory.atomic.wait32` whose expected value differs from the cell returns
    // 1 ("not equal") immediately, without blocking — even on a shared memory.
    compile_run_raw(
        "atomics_threads_wait_mismatch",
        r#"(module
            (memory 1 1 shared)
            (func (export "w") (result i32)
              i32.const 0    ;; addr
              i32.const 5    ;; expected (cell is 0)
              i64.const -1   ;; infinite timeout
              memory.atomic.wait32))"#,
        r#"fn main() {
            let mut inst = Instance::new();
            assert_eq!(inst.func0(), 1);
        }"#,
    );
}

#[test]
fn notify_with_no_waiters_returns_zero() {
    // `memory.atomic.notify` on an address nobody waits on wakes zero waiters.
    compile_run_raw(
        "atomics_threads_notify_empty",
        r#"(module
            (memory 1 1 shared)
            (func (export "n") (result i32)
              i32.const 0    ;; addr
              i32.const 1    ;; count
              memory.atomic.notify))"#,
        r#"fn main() {
            let mut inst = Instance::new();
            assert_eq!(inst.func0(), 0);
        }"#,
    );
}

#[test]
fn wait_blocks_until_notified() {
    // A real handshake: a waiter thread parks on `wait32(addr=0, expected=0)`
    // while the cell is 0; the main thread later stores 1 and notifies, waking
    // the waiter, whose `wait32` returns 0 ("ok"). A generous timeout keeps a
    // regression from hanging the suite forever — a broken notify returns 2.
    compile_run_raw(
        "atomics_threads_wait_notify",
        r#"(module
            (memory 1 1 shared)
            (func (export "wait_ok") (result i32)
              i32.const 0
              i32.const 0
              i64.const 2000000000   ;; 2s timeout in ns
              memory.atomic.wait32)
            (func (export "store1")
              i32.const 0 i32.const 1 i32.atomic.store)
            (func (export "notify1") (result i32)
              i32.const 0 i32.const 1 memory.atomic.notify))"#,
        r#"fn main() {
            let inst = Instance::new();
            let handle = inst.shared_memory();
            let m = handle.clone();
            let waiter = std::thread::spawn(move || {
                let mut t = Instance::with_memory(m);
                t.func0() // wait_ok -> 0 when woken, 2 if it timed out
            });
            // Give the waiter time to park, then store and notify.
            std::thread::sleep(std::time::Duration::from_millis(200));
            let mut w = Instance::with_memory(handle);
            w.func1();               // store 1
            let woken = w.func2();   // notify one
            assert_eq!(woken, 1, "expected to wake exactly one waiter");
            assert_eq!(waiter.join().unwrap(), 0, "waiter should return 0 (woken), not 2 (timeout)");
        }"#,
    );
}
