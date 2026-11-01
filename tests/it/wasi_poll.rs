//! Native `poll_oneoff`, `sock_*` and `proc_raise`. The generated Rust is
//! compiled with `rustc -D warnings` and run (no mocking).

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

/// A 48-byte `subscription` for a clock: `userdata`, tag 0, then the clock id,
/// timeout (ns), precision and flags.
fn clock_sub(userdata: u64, id: u32, timeout: u64, flags: u16) -> Vec<u8> {
    let mut s = vec![0u8; 48];
    s[0..8].copy_from_slice(&userdata.to_le_bytes());
    s[16..20].copy_from_slice(&id.to_le_bytes());
    s[24..32].copy_from_slice(&timeout.to_le_bytes());
    s[40..42].copy_from_slice(&flags.to_le_bytes());
    s
}

/// A 48-byte `subscription` for `fd_read` (tag 1) or `fd_write` (tag 2).
fn fd_sub(userdata: u64, tag: u8, fd: u32) -> Vec<u8> {
    let mut s = vec![0u8; 48];
    s[0..8].copy_from_slice(&userdata.to_le_bytes());
    s[8] = tag;
    s[16..20].copy_from_slice(&fd.to_le_bytes());
    s
}

/// A module that polls `subs` (copied to address 0) and exposes the result:
/// `func1` returns the errno, `func2` the event count, and `func3..func6` the
/// first event's userdata, error, type and nbytes.
fn poll_module(subs: &[Vec<u8>]) -> String {
    let bytes: String = subs
        .iter()
        .flatten()
        .map(|b| format!("\\{b:02x}"))
        .collect();
    let n = subs.len();
    format!(
        r#"(module
            (import "wasi_snapshot_preview1" "poll_oneoff"
              (func $poll (param i32 i32 i32 i32) (result i32)))
            (memory 1)
            (data (i32.const 0) "{bytes}")
            (func (export "run") (result i32)
              i32.const 0 i32.const 512 i32.const {n} i32.const 1024
              call $poll)
            (func (export "nevents") (result i32) i32.const 1024 i32.load)
            (func (export "userdata") (result i64) i32.const 512 i64.load)
            (func (export "error") (result i32) i32.const 520 i32.load16_u)
            (func (export "type") (result i32) i32.const 522 i32.load8_u)
            (func (export "nbytes") (result i64) i32.const 528 i64.load))"#
    )
}

#[test]
fn a_relative_clock_subscription_sleeps_and_reports_one_event() {
    compile_run(
        "poll_clock_sleep",
        &poll_module(&[clock_sub(7, 1, 60_000_000, 0)]),
        r#"let mut inst = Instance::new();
        let start = std::time::Instant::now();
        assert_eq!(inst.func1(), 0);
        assert!(start.elapsed() >= std::time::Duration::from_millis(60));
        assert_eq!(inst.func2(), 1);
        assert_eq!(inst.func3(), 7);
        assert_eq!(inst.func4(), 0);
        assert_eq!(inst.func5(), 0);"#,
    );
}

#[test]
fn the_earliest_of_several_clocks_wakes_the_call() {
    compile_run(
        "poll_clock_earliest",
        &poll_module(&[
            clock_sub(1, 1, 10_000_000_000, 0),
            clock_sub(2, 1, 40_000_000, 0),
        ]),
        r#"let mut inst = Instance::new();
        let start = std::time::Instant::now();
        assert_eq!(inst.func1(), 0);
        assert!(start.elapsed() >= std::time::Duration::from_millis(40));
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
        assert_eq!(inst.func2(), 1);
        assert_eq!(inst.func3(), 2);"#,
    );
}

#[test]
fn an_absolute_clock_in_the_past_returns_immediately() {
    compile_run(
        "poll_clock_absolute_past",
        &poll_module(&[clock_sub(5, 0, 1, 1)]),
        r#"let mut inst = Instance::new();
        let start = std::time::Instant::now();
        assert_eq!(inst.func1(), 0);
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
        assert_eq!(inst.func2(), 1);
        assert_eq!(inst.func3(), 5);
        assert_eq!(inst.func4(), 0);"#,
    );
}

#[test]
fn an_unknown_clock_id_is_reported_as_einval_on_its_event() {
    compile_run(
        "poll_clock_bad_id",
        &poll_module(&[clock_sub(3, 9, 1_000_000, 0)]),
        r#"let mut inst = Instance::new();
        assert_eq!(inst.func1(), 0);
        assert_eq!(inst.func2(), 1);
        assert_eq!(inst.func3(), 3);
        assert_eq!(inst.func4(), 28);
        assert_eq!(inst.func5(), 0);"#,
    );
}

#[test]
fn a_ready_fd_returns_without_waiting_for_a_long_clock() {
    // stdout is always writable, so the 10 s clock must not be slept.
    compile_run(
        "poll_fd_write_ready",
        &poll_module(&[clock_sub(1, 1, 10_000_000_000, 0), fd_sub(9, 2, 1)]),
        r#"let mut inst = Instance::new();
        let start = std::time::Instant::now();
        assert_eq!(inst.func1(), 0);
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
        assert_eq!(inst.func2(), 1);
        assert_eq!(inst.func3(), 9);
        assert_eq!(inst.func4(), 0);
        assert_eq!(inst.func5(), 2);
        assert_eq!(inst.func6(), 0);"#,
    );
}

#[test]
fn stdin_is_always_reported_readable() {
    compile_run(
        "poll_stdin_ready",
        &poll_module(&[clock_sub(1, 1, 10_000_000_000, 0), fd_sub(4, 1, 0)]),
        r#"let mut inst = Instance::new();
        assert_eq!(inst.func1(), 0);
        assert_eq!(inst.func2(), 1);
        assert_eq!(inst.func3(), 4);
        assert_eq!(inst.func4(), 0);
        assert_eq!(inst.func5(), 1);"#,
    );
}

#[test]
fn an_unopened_fd_is_an_ebadf_event() {
    compile_run(
        "poll_fd_ebadf",
        &poll_module(&[fd_sub(6, 1, 99)]),
        r#"let mut inst = Instance::new();
        assert_eq!(inst.func1(), 0);
        assert_eq!(inst.func2(), 1);
        assert_eq!(inst.func3(), 6);
        assert_eq!(inst.func4(), 8);
        assert_eq!(inst.func5(), 1);"#,
    );
}

#[test]
fn zero_subscriptions_is_einval() {
    compile_run(
        "poll_zero_subs",
        r#"(module
            (import "wasi_snapshot_preview1" "poll_oneoff"
              (func $poll (param i32 i32 i32 i32) (result i32)))
            (memory 1)
            (func (export "run") (result i32)
              i32.const 0 i32.const 512 i32.const 0 i32.const 1024
              call $poll))"#,
        "let mut inst = Instance::new(); assert_eq!(inst.func1(), 28);",
    );
}

#[test]
fn poll_oneoff_works_from_threads_on_a_shared_memory() {
    // Several threads poll a short clock on one shared memory at once: the call
    // must not hold the memory lock while it sleeps.
    let bytes: String = clock_sub(1, 1, 20_000_000, 0)
        .iter()
        .map(|b| format!("\\{b:02x}"))
        .collect();
    compile_run(
        "poll_shared_threads",
        &format!(
            r#"(module
            (import "wasi_snapshot_preview1" "poll_oneoff"
              (func $poll (param i32 i32 i32 i32) (result i32)))
            (memory 1 1 shared)
            (data (i32.const 0) "{bytes}")
            (func (export "run") (result i32)
              i32.const 0 i32.const 512 i32.const 1 i32.const 1024
              call $poll))"#
        ),
        r#"let first = Instance::new();
        let handle = first.shared_memory();
        let mut joins = Vec::new();
        for _ in 0..4 {
            let m = handle.clone();
            joins.push(std::thread::spawn(move || {
                let mut t = Instance::with_memory(m);
                for _ in 0..3 { assert_eq!(t.func1(), 0); }
            }));
        }
        for j in joins { j.join().unwrap(); }"#,
    );
}

#[test]
fn sock_functions_report_badf_or_notsock() {
    // fd 99 is not open (EBADF 8); stdout (fd 1) is open but not a socket
    // (ENOTSOCK 57).
    compile_run(
        "sock_stubs",
        r#"(module
            (import "wasi_snapshot_preview1" "sock_accept"
              (func $accept (param i32 i32 i32) (result i32)))
            (import "wasi_snapshot_preview1" "sock_recv"
              (func $recv (param i32 i32 i32 i32 i32 i32) (result i32)))
            (import "wasi_snapshot_preview1" "sock_send"
              (func $send (param i32 i32 i32 i32 i32) (result i32)))
            (import "wasi_snapshot_preview1" "sock_shutdown"
              (func $shutdown (param i32 i32) (result i32)))
            (func (export "accept") (param i32) (result i32)
              local.get 0 i32.const 0 i32.const 0 call $accept)
            (func (export "recv") (param i32) (result i32)
              local.get 0 i32.const 0 i32.const 0 i32.const 0 i32.const 0 i32.const 0
              call $recv)
            (func (export "send") (param i32) (result i32)
              local.get 0 i32.const 0 i32.const 0 i32.const 0 i32.const 0 call $send)
            (func (export "shutdown") (param i32) (result i32)
              local.get 0 i32.const 2 call $shutdown))"#,
        r#"let mut inst = Instance::new();
        assert_eq!(inst.func4(1), 57);
        assert_eq!(inst.func5(1), 57);
        assert_eq!(inst.func6(1), 57);
        assert_eq!(inst.func7(1), 57);
        assert_eq!(inst.func4(99), 8);
        assert_eq!(inst.func5(99), 8);
        assert_eq!(inst.func6(99), 8);
        assert_eq!(inst.func7(99), 8);"#,
    );
}

#[test]
fn proc_raise_validates_the_signal_and_is_unsupported() {
    compile_run(
        "proc_raise",
        r#"(module
            (import "wasi_snapshot_preview1" "proc_raise"
              (func $raise (param i32) (result i32)))
            (func (export "run") (param i32) (result i32)
              local.get 0 call $raise))"#,
        r#"let mut inst = Instance::new();
        assert_eq!(inst.func1(0), 28);
        assert_eq!(inst.func1(32), 28);
        assert_eq!(inst.func1(-1), 28);
        assert_eq!(inst.func1(9), 52);"#,
    );
}

#[test]
fn with_a_file_table_the_preopen_is_ready_and_an_unopened_slot_is_ebadf() {
    // `fd_prestat_get` gives the module an fd table: fd 3 (the preopen) is open,
    // fd 4 is not.
    let bytes: String = [fd_sub(1, 1, 3), fd_sub(2, 1, 4)]
        .iter()
        .flatten()
        .map(|b| format!("\\{b:02x}"))
        .collect();
    compile_run(
        "poll_file_table",
        &format!(
            r#"(module
            (import "wasi_snapshot_preview1" "poll_oneoff"
              (func $poll (param i32 i32 i32 i32) (result i32)))
            (import "wasi_snapshot_preview1" "fd_prestat_get"
              (func $prestat (param i32 i32) (result i32)))
            (memory 1)
            (data (i32.const 0) "{bytes}")
            (func (export "run") (result i32)
              i32.const 0 i32.const 512 i32.const 2 i32.const 1024
              call $poll)
            (func (export "error0") (result i32) i32.const 520 i32.load16_u)
            (func (export "error1") (result i32) i32.const 552 i32.load16_u))"#
        ),
        r#"let mut inst = Instance::new();
        assert_eq!(inst.func2(), 0);
        assert_eq!(inst.func3(), 0);
        assert_eq!(inst.func4(), 8);"#,
    );
}
