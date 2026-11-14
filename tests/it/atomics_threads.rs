//! End-to-end tests for *real* multi-threaded atomics over a `shared` memory.
//!
//! When a module's memory is declared `shared`, wasm2rs backs it with a
//! thread-shareable handle (`SharedMemory`, cheaply clonable, `Send + Sync`).
//! Sibling instances built with `Instance::with_memory(handle)` share the same
//! linear memory (each keeps its own globals/tables), so atomic operations issued
//! from different OS threads are genuinely atomic and `wait`/`notify` actually
//! block and wake. Non-shared memory keeps the fast single-threaded path (see
//! tests/atomics.rs), which these tests do not touch.

use crate::common;

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
fn shared_memory_grow_respects_declared_maximum_across_instances() {
    // A shared `(memory 1 2)`: a sibling instance sees the same size. Growing to
    // exactly the maximum succeeds, growing past it fails from either instance and
    // leaves the size unchanged.
    compile_run_raw(
        "atomics_threads_grow_declared_max",
        r#"(module
            (memory 1 2 shared)
            (func (param i32) (result i32) local.get 0 memory.grow)
            (func (result i32) memory.size))"#,
        r#"fn main() {
            let mut first = Instance::new();
            let mut sibling = Instance::with_memory(first.shared_memory());
            assert_eq!(first.func0(2), -1);
            assert_eq!(first.func1(), 1);
            assert_eq!(first.func0(1), 1);
            assert_eq!(sibling.func1(), 2);
            assert_eq!(sibling.func0(1), -1);
            assert_eq!(first.func0(0), 2);
            assert_eq!(first.func1(), 2);
        }"#,
    );
}

#[test]
fn shared_memory_concurrent_grows_never_pass_the_declared_maximum() {
    // Four threads race to grow a shared `(memory 1 5)` by one page, ten times
    // each: exactly four succeed (returning distinct old sizes) and the final size
    // is the maximum.
    compile_run_raw(
        "atomics_threads_grow_race_max",
        r#"(module
            (memory 1 5 shared)
            (func (export "grow") (result i32) i32.const 1 memory.grow)
            (func (export "size") (result i32) memory.size))"#,
        r#"fn main() {
            let mut first = Instance::new();
            let handle = first.shared_memory();
            let threads: Vec<_> = (0..4)
                .map(|_| {
                    let h = handle.clone();
                    std::thread::spawn(move || {
                        let mut inst = Instance::with_memory(h);
                        (0..10).map(|_| inst.func0()).filter(|r| *r >= 0).collect::<Vec<i32>>()
                    })
                })
                .collect();
            let mut won: Vec<i32> = threads.into_iter().flat_map(|t| t.join().unwrap()).collect();
            won.sort();
            assert_eq!(won, vec![1, 2, 3, 4]);
            assert_eq!(first.func1(), 5);
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

#[test]
fn notify_one_wakes_exactly_one_of_two_waiters() {
    // Two waiters park on the same address; `notify(1)` must wake exactly one
    // (returning 1). The other stays parked until its 1s timeout (returns 2).
    // Waking both would yield [0, 0].
    compile_run_raw(
        "atomics_threads_notify_one_of_two",
        r#"(module
            (memory 1 1 shared)
            (func (export "wait_1s") (result i32)
              i32.const 0
              i32.const 0
              i64.const 1000000000   ;; 1s timeout in ns
              memory.atomic.wait32)
            (func (export "notify1") (result i32)
              i32.const 0 i32.const 1 memory.atomic.notify))"#,
        r#"fn main() {
            let inst = Instance::new();
            let handle = inst.shared_memory();
            let mut joins = Vec::new();
            for _ in 0..2 {
                let m = handle.clone();
                joins.push(std::thread::spawn(move || {
                    let mut t = Instance::with_memory(m);
                    t.func0()
                }));
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
            let mut w = Instance::with_memory(handle);
            assert_eq!(w.func1(), 1, "notify(1) reports one woken waiter");
            let mut results: Vec<i32> = joins.into_iter().map(|j| j.join().unwrap()).collect();
            results.sort();
            assert_eq!(results, vec![0, 2], "exactly one waiter woken, the other times out");
        }"#,
    );
}

#[test]
fn imported_shared_memory_is_shared_across_threads() {
    // The memory is imported (`env.memory`, shared): the host creates the
    // `SharedMemory` and passes it to `Instance::new`; sibling threads join it
    // with `with_memory`. Atomic adds from eight threads must all land.
    compile_run_raw(
        "atomics_threads_imported_rmw",
        r#"(module
            (import "env" "memory" (memory 1 1 shared))
            (func (export "inc")
              i32.const 0 i32.const 1 i32.atomic.rmw.add drop)
            (func (export "get") (result i32)
              i32.const 0 i32.atomic.load))"#,
        r#"fn main() {
            let handle = SharedMemory::new(65536);
            let _first = Instance::new(handle.clone());
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
fn imported_shared_memory_data_is_applied_once_by_new() {
    // `new` applies the active data segment into the host-provided memory;
    // `with_memory` must not re-apply it (the cell was changed in between).
    compile_run_raw(
        "atomics_threads_imported_data",
        r#"(module
            (import "env" "memory" (memory 1 1 shared))
            (data (i32.const 0) "\07\00\00\00")
            (func (export "get") (result i32)
              i32.const 0 i32.atomic.load)
            (func (export "set9")
              i32.const 0 i32.const 9 i32.atomic.store))"#,
        r#"fn main() {
            let handle = SharedMemory::new(65536);
            let mut a = Instance::new(handle.clone());
            assert_eq!(a.func0(), 7, "new applied the data segment");
            a.func1();
            let mut b = Instance::with_memory(handle);
            assert_eq!(b.func0(), 9, "with_memory must not re-apply data");
        }"#,
    );
}

#[test]
fn imported_shared_memory_with_a_host_import_takes_memory_and_imports() {
    // The wasi-threads shape: a shared `env.memory` *and* a host function
    // import. `new(memory, imports)` / `with_memory(memory, imports)` must both
    // compile and run; each thread brings its own host value. `inc` adds
    // `double(1)` = 2 atomically, so four threads x 1_000 calls total 8_000.
    compile_run_raw(
        "atomics_threads_imported_with_host",
        r#"(module
            (import "env" "memory" (memory 1 1 shared))
            (import "env" "double" (func (param i32) (result i32)))
            (func (export "inc")
              i32.const 0
              i32.const 1
              call 0
              i32.atomic.rmw.add
              drop)
            (func (export "get") (result i32)
              i32.const 0 i32.atomic.load))"#,
        r#"struct Host;
        impl Imports for Host {
            fn import0(&mut self, a0: i32) -> i32 { a0 * 2 }
        }
        fn main() {
            let handle = SharedMemory::new(65536);
            let _first = Instance::new(handle.clone(), Host);
            let threads = 4;
            let per = 1_000;
            let mut joins = Vec::new();
            for _ in 0..threads {
                let m = handle.clone();
                joins.push(std::thread::spawn(move || {
                    let mut t = Instance::with_memory(m, Host);
                    for _ in 0..per { t.func1(); }
                }));
            }
            for j in joins { j.join().unwrap(); }
            let mut reader = Instance::with_memory(handle, Host);
            assert_eq!(reader.func2(), 2 * threads * per);
        }"#,
    );
}

#[test]
fn a_misaligned_atomic_access_traps() {
    // The threads proposal traps an atomic access whose effective address is not
    // a multiple of its width (a plain access has no such rule).
    compile_run_raw(
        "atomics_threads_misaligned",
        r#"(module
            (memory 1 1 shared)
            (func (export "load") (result i32)
              i32.const 2 i32.atomic.load)
            (func (export "store")
              i32.const 6 i32.const 1 i32.atomic.store)
            (func (export "rmw") (result i32)
              i32.const 1 i32.const 1 i32.atomic.rmw.add)
            (func (export "cmpxchg") (result i32)
              i32.const 3 i32.const 0 i32.const 1 i32.atomic.rmw.cmpxchg)
            (func (export "rmw16_ok") (result i32)
              i32.const 2 i32.const 5 i32.atomic.rmw16.add_u)
            (func (export "plain_ok") (result i32)
              i32.const 2 i32.load))"#,
        r#"fn main() {
            use std::panic::{catch_unwind, AssertUnwindSafe};
            let mut inst = Instance::new();
            assert!(catch_unwind(AssertUnwindSafe(|| inst.func0())).is_err(), "load");
            assert!(catch_unwind(AssertUnwindSafe(|| inst.func1())).is_err(), "store");
            assert!(catch_unwind(AssertUnwindSafe(|| inst.func2())).is_err(), "rmw");
            assert!(catch_unwind(AssertUnwindSafe(|| inst.func3())).is_err(), "cmpxchg");
            assert_eq!(inst.func4(), 0, "an aligned narrow access is fine");
            assert_eq!(inst.func5(), 5, "a plain access may be unaligned");
        }"#,
    );
}

#[test]
fn growing_a_shared_memory_stops_at_its_declared_maximum() {
    // `(memory 1 3 shared)`: growth succeeds up to 3 pages, then fails with -1,
    // and a thread on the same memory sees the grown size.
    compile_run_raw(
        "atomics_threads_grow_max",
        r#"(module
            (memory 1 3 shared)
            (func (export "grow") (param i32) (result i32)
              local.get 0 memory.grow)
            (func (export "size") (result i32) memory.size))"#,
        r#"fn main() {
            let mut a = Instance::new();
            let handle = a.shared_memory();
            assert_eq!(a.func0(2), 1, "old size in pages");
            assert_eq!(a.func0(1), -1, "past the declared maximum");
            assert_eq!(a.func1(), 3);
            let seen = std::thread::spawn(move || Instance::with_memory(handle).func1())
                .join()
                .unwrap();
            assert_eq!(seen, 3, "another instance sees the grown memory");
        }"#,
    );
}

#[test]
fn a_spin_on_atomic_load_observes_another_threads_atomic_store() {
    // The waiter loops on `i32.atomic.load` until the flag becomes 1. A plain
    // (hoistable) load would spin forever, so this only terminates if the load
    // really re-reads shared memory. The flag publishes `data` stored before it.
    compile_run_raw(
        "atomics_threads_spin",
        r#"(module
            (memory 1 1 shared)
            (func (export "spin") (result i32)
              (loop $l
                (br_if $l (i32.eqz (i32.atomic.load (i32.const 0)))))
              (i32.load (i32.const 8)))
            (func (export "publish")
              (i32.store (i32.const 8) (i32.const 77))
              (i32.atomic.store (i32.const 0) (i32.const 1))))"#,
        r#"fn main() {
            let inst = Instance::new();
            let handle = inst.shared_memory();
            let m = handle.clone();
            let waiter = std::thread::spawn(move || Instance::with_memory(m).func0());
            std::thread::sleep(std::time::Duration::from_millis(100));
            Instance::with_memory(handle).func1();
            assert_eq!(waiter.join().unwrap(), 77);
        }"#,
    );
}

#[test]
fn threads_writing_disjoint_regions_keep_every_byte() {
    // Four threads each fill their own 16 KiB region with a per-thread byte
    // pattern, concurrently; afterwards every region holds exactly its pattern.
    compile_run_raw(
        "atomics_threads_disjoint",
        r#"(module
            (memory 1 1 shared)
            (func (export "fill") (param $base i32) (param $v i32)
              (local $i i32)
              (loop $l
                (i32.store8 (i32.add (local.get $base) (local.get $i)) (local.get $v))
                (local.set $i (i32.add (local.get $i) (i32.const 1)))
                (br_if $l (i32.lt_u (local.get $i) (i32.const 16384)))))
            (func (export "at") (param i32) (result i32)
              local.get 0 i32.load8_u))"#,
        r#"fn main() {
            let inst = Instance::new();
            let handle = inst.shared_memory();
            let joins: Vec<_> = (0..4).map(|k| {
                let m = handle.clone();
                std::thread::spawn(move || {
                    Instance::with_memory(m).func0(k * 16384, 10 + k);
                })
            }).collect();
            for j in joins { j.join().unwrap(); }
            let mut r = Instance::with_memory(handle);
            for k in 0..4 {
                for off in [0, 1, 8191, 16383] {
                    assert_eq!(r.func1(k * 16384 + off), 10 + k);
                }
            }
        }"#,
    );
}

#[test]
fn an_atomic_load_is_evaluated_in_program_order_even_when_dropped_or_fenced() {
    // Sequential consistency needs `atomic.load x; atomic.load y` to read x
    // first, and a load before an `atomic.fence` to be evaluated before it, so
    // none of them may be left pending on the operand stack.
    let wasm = wat::parse_str(
        r#"(module
            (memory 1 1 shared)
            (func (export "drops")
              i32.const 100 i32.atomic.load
              i32.const 200 i32.atomic.load
              drop drop)
            (func (export "fenced")
              i32.const 300 i32.load
              atomic.fence
              i32.const 0 i32.const 1 i32.store
              drop))"#,
    );
    let wasm = wasm.expect("valid wat");
    let code = wasm2rs::transpile(&wasm).expect("transpile ok");
    let at = |needle: &str| {
        code.find(needle)
            .unwrap_or_else(|| panic!("no {needle} in:\n{code}"))
    };
    assert!(
        at("(100i32)") < at("(200i32)"),
        "atomic loads read out of order:\n{code}"
    );
    assert!(
        at("(300i32,") < at("fence("),
        "the load before the fence must not move after it:\n{code}"
    );
}

#[test]
fn a_trapping_wait_does_not_poison_later_waits_and_notifies() {
    // A `wait` on a misaligned address traps; another thread's `notify` (and a
    // fresh `wait`) on the same memory must still work afterwards.
    compile_run_raw(
        "atomics_threads_wait_trap",
        r#"(module
            (memory 1 1 shared)
            (func (export "bad_wait") (result i32)
              i32.const 2 i32.const 0 i64.const 0 memory.atomic.wait32)
            (func (export "ok_wait") (result i32)
              i32.const 8 i32.const 5 i64.const 0 memory.atomic.wait32)
            (func (export "notify") (result i32)
              i32.const 8 i32.const 1 memory.atomic.notify))"#,
        r#"fn main() {
            use std::panic::{catch_unwind, AssertUnwindSafe};
            let mut inst = Instance::new();
            assert!(catch_unwind(AssertUnwindSafe(|| inst.func0())).is_err());
            assert_eq!(inst.func1(), 1, "value mismatch still returns 1");
            assert_eq!(inst.func2(), 0, "notify still works");
        }"#,
    );
}

#[test]
fn shared_plain_access_reads_len_relaxed() {
    // A plain access only needs the length as a bounds limit: `len` never
    // shrinks and the bytes are zeroed up front, so there is nothing to acquire.
    // An Acquire load would cost a load-acquire on every plain access.
    let wasm = wat::parse_str(
        r#"(module (memory 1 2 shared) (func (export "f") (result i32) i32.const 0 i32.load))"#,
    )
    .unwrap();
    let src = wasm2rs::transpile(&wasm).unwrap();
    let start = src.find("fn bytes(&self)").expect("bytes() emitted");
    // The load is the first statement of `bytes()`; check just that line.
    let load = src[start..]
        .lines()
        .find(|l| l.contains("self.0.len.load"))
        .expect("len load in bytes()");
    assert!(load.contains("Ordering::Relaxed"), "{load}");
}

#[test]
fn shared_grow_by_one_thread_is_usable_by_another_after_handshake() {
    // Thread A grows the memory then publishes a flag with an atomic store;
    // thread B waits on the flag (SeqCst) and then plain-stores/loads in the
    // grown page. The handshake orders A's `len` update before B's plain access,
    // so a Relaxed `len` read in the plain path must still see the new length.
    // (Documents the happy path; the ordering itself is pinned by the source test.)
    compile_run_raw(
        "atomics_threads_grow_handshake_plain",
        r#"(module
            (memory 1 2 shared)
            (func (export "grow_and_flag")
              i32.const 1 memory.grow drop
              i32.const 0 i32.const 1 i32.atomic.store)
            (func (export "flag") (result i32) i32.const 0 i32.atomic.load)
            (func (export "poke") (result i32)
              i32.const 65540 i32.const 77 i32.store
              i32.const 65540 i32.load))"#,
        r#"fn main() {
            let first = Instance::new();
            let handle = first.shared_memory();
            let h = handle.clone();
            let a = std::thread::spawn(move || Instance::with_memory(h).func0());
            let h = handle.clone();
            let b = std::thread::spawn(move || {
                let mut inst = Instance::with_memory(h);
                while inst.func1() == 0 { std::thread::yield_now(); }
                inst.func2()
            });
            a.join().unwrap();
            assert_eq!(b.join().unwrap(), 77);
        }"#,
    );
}
