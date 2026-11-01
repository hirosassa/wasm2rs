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

use common::{compile_run, compile_run_raw, compile_run_split, expect_trap_with};
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

const FS_IMPORTS: &str = r#"
      (import "env" "memory" (memory 1 1 shared))
      (import "wasi" "thread-spawn" (func $spawn (param i32) (result i32)))
      (import "wasi_snapshot_preview1" "path_open"
        (func $po (param i32 i32 i32 i32 i32 i64 i64 i32 i32) (result i32)))
      (import "wasi_snapshot_preview1" "fd_write"
        (func $wr (param i32 i32 i32 i32) (result i32)))"#;

#[test]
fn a_child_can_use_a_descriptor_the_parent_opened() {
    // The parent creates "out.txt" (fd 4) and hands the fd to a child, which
    // writes to it and reports the errno at 312. The descriptor table is shared
    // by every thread, so the write must succeed and reach the file.
    compile_run(
        "wasi_threads_shared_fd",
        &format!(
            r#"(module {FS_IMPORTS}
              (data (i32.const 100) "out.txt")
              (data (i32.const 0) "\c8\00\00\00\09\00\00\00")
              (data (i32.const 200) "persisted")
              (func (export "wasi_thread_start") (param i32 i32)
                i32.const 312
                local.get 1
                i32.const 0
                i32.const 1
                i32.const 308
                call $wr
                i32.atomic.store
                i32.const 400
                i32.const 1
                i32.atomic.store
                i32.const 400
                i32.const 1
                memory.atomic.notify
                drop)
              (func (export "run") (result i32)
                i32.const 3 i32.const 0 i32.const 100 i32.const 7
                i32.const 9 i64.const 64 i64.const 0 i32.const 0 i32.const 300
                call $po
                if unreachable end
                i32.const 300
                i32.load
                call $spawn
                drop
                i32.const 400
                i32.const 0
                i64.const 5000000000
                memory.atomic.wait32
                drop
                i32.const 312
                i32.atomic.load))"#
        ),
        r#"let dir = std::env::temp_dir().join(format!("wasm2rs_shared_fd_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut inst = Instance::new(SharedMemory::new(65536)).with_preopen_root(&dir);
        assert_eq!(inst.func4(), 0, "the child's fd_write must succeed");
        assert_eq!(std::fs::read(dir.join("out.txt")).unwrap(), b"persisted");
        std::fs::remove_dir_all(&dir).ok();"#,
    );
}

#[test]
fn concurrent_path_opens_get_distinct_descriptors() {
    // Four children each open the same file and record `1 << fd` in a mask at
    // 500. With one shared table the descriptors are 4..=7 (mask 0xf0); a slot
    // handed out twice, or a per-thread table, would leave bits unset.
    compile_run(
        "wasi_threads_distinct_fds",
        &format!(
            r#"(module {FS_IMPORTS}
              (data (i32.const 100) "same.txt")
              (func (export "wasi_thread_start") (param i32 i32)
                i32.const 3 i32.const 0 i32.const 100 i32.const 8
                i32.const 1 i64.const 64 i64.const 0 i32.const 0
                local.get 0 i32.const 4 i32.mul i32.const 600 i32.add
                call $po
                if unreachable end
                i32.const 500
                i32.const 1
                local.get 0 i32.const 4 i32.mul i32.const 600 i32.add
                i32.load
                i32.shl
                i32.atomic.rmw.or
                drop
                i32.const 504
                i32.const 1
                i32.atomic.rmw.add
                drop
                i32.const 504
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
                  i32.const 504
                  i32.atomic.load
                  i32.const 4
                  i32.ne
                  if
                    i32.const 504
                    i32.const 0
                    i64.const 5000000000
                    memory.atomic.wait32
                    drop
                    br $again
                  end)
                i32.const 500
                i32.atomic.load))"#
        ),
        r#"let dir = std::env::temp_dir().join(format!("wasm2rs_distinct_fds_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut inst = Instance::new(SharedMemory::new(65536)).with_preopen_root(&dir);
        assert_eq!(inst.func5(), 0xf0);
        std::fs::remove_dir_all(&dir).ok();"#,
    );
}

#[test]
fn every_descriptor_table_function_compiles_with_thread_spawn() {
    // Each WASI function that touches `wasi_fds` is rewritten to lock the shared
    // table; the whole set must compile together under `-D warnings`.
    let funcs: &[(&str, &[&str])] = &[
        ("fd_write", &["i32", "i32", "i32", "i32"]),
        ("fd_read", &["i32", "i32", "i32", "i32"]),
        ("fd_close", &["i32"]),
        ("fd_seek", &["i32", "i64", "i32", "i32"]),
        ("fd_fdstat_get", &["i32", "i32"]),
        (
            "path_open",
            &[
                "i32", "i32", "i32", "i32", "i32", "i64", "i64", "i32", "i32",
            ],
        ),
        ("fd_prestat_get", &["i32", "i32"]),
        ("fd_prestat_dir_name", &["i32", "i32", "i32"]),
        ("fd_filestat_get", &["i32", "i32"]),
        ("fd_pread", &["i32", "i32", "i32", "i64", "i32"]),
        ("fd_pwrite", &["i32", "i32", "i32", "i64", "i32"]),
        ("fd_readdir", &["i32", "i32", "i32", "i64", "i32"]),
    ];
    let imports: String = funcs
        .iter()
        .map(|(name, params)| {
            format!(
                "(import \"wasi_snapshot_preview1\" \"{name}\" (func (param {}) (result i32)))\n",
                params.join(" ")
            )
        })
        .collect();
    compile_run(
        "wasi_threads_every_fd_function",
        &format!(
            r#"(module {SPAWN_IMPORT} {imports}
              (func (export "wasi_thread_start") (param i32 i32)))"#
        ),
        "let _inst = Instance::new(SharedMemory::new(65536));",
    );
}

#[test]
fn a_child_does_not_reapply_the_parents_data_segments() {
    // The data segment puts 7 at address 8. The parent overwrites it with 99
    // before spawning; a child instance must not re-run the segment, so it sees
    // 99 and copies it to address 0.
    compile_run(
        "wasi_threads_data_not_reapplied",
        &format!(
            r#"(module {SPAWN_IMPORT}
              (data (i32.const 8) "\07\00\00\00")
              (func (export "wasi_thread_start") (param i32 i32)
                i32.const 0
                i32.const 8
                i32.atomic.load
                i32.atomic.store
                i32.const 0
                i32.const 1
                memory.atomic.notify
                drop)
              (func (export "run") (result i32)
                i32.const 8 i32.const 99 i32.atomic.store
                i32.const 0
                call $spawn
                drop
                i32.const 0 i32.const 0 i64.const 5000000000
                memory.atomic.wait32
                drop
                i32.const 0
                i32.atomic.load))"#
        ),
        "let mut inst = Instance::new(SharedMemory::new(65536)); assert_eq!(inst.func2(), 99);",
    );
}

#[test]
fn a_child_runs_nested_loops_and_branches() {
    // The child sums i*j for i, j in 1..=20 with nested loops, blocks and a
    // conditional skip, then publishes the total (2 * 210 * 210 - skipped).
    compile_run(
        "wasi_threads_loops",
        &format!(
            r#"(module {SPAWN_IMPORT}
              (func $sum (result i32)
                (local $i i32) (local $j i32) (local $acc i32)
                i32.const 1 local.set $i
                block $done
                  loop $outer
                    local.get $i i32.const 20 i32.gt_s br_if $done
                    i32.const 1 local.set $j
                    block $inner_done
                      loop $inner
                        local.get $j i32.const 20 i32.gt_s br_if $inner_done
                        local.get $j i32.const 5 i32.rem_s
                        if
                        else
                          local.get $acc local.get $i local.get $j i32.mul i32.add local.set $acc
                        end
                        local.get $j i32.const 1 i32.add local.set $j
                        br $inner
                      end
                    end
                    local.get $i i32.const 1 i32.add local.set $i
                    br $outer
                  end
                end
                local.get $acc)
              (func (export "wasi_thread_start") (param i32 i32)
                i32.const 0 call $sum i32.atomic.store
                i32.const 0 i32.const 1 memory.atomic.notify drop)
              (func (export "run") (result i32)
                i32.const 0 call $spawn drop
                i32.const 0 i32.const 0 i64.const 5000000000
                memory.atomic.wait32 drop
                i32.const 0 i32.atomic.load))"#
        ),
        // multiples of 5 for j: 5,10,15,20 sum 50; times sum(i)=210.
        "let mut inst = Instance::new(SharedMemory::new(65536)); assert_eq!(inst.func3(), 50 * 210);",
    );
}

#[test]
fn a_child_can_resume_a_continuation() {
    compile_run(
        "wasi_threads_cont",
        &format!(
            r#"(module {SPAWN_IMPORT}
              (type $ft (func (result i32)))
              (type $ct (cont $ft))
              (func $gen (result i32) i32.const 42)
              (func (export "wasi_thread_start") (param i32 i32)
                i32.const 0
                ref.func $gen cont.new $ct resume $ct
                i32.atomic.store
                i32.const 0 i32.const 1 memory.atomic.notify drop)
              (func (export "run") (result i32)
                i32.const 0 call $spawn drop
                i32.const 0 i32.const 0 i64.const 5000000000
                memory.atomic.wait32 drop
                i32.const 0 i32.atomic.load))"#
        ),
        "let mut inst = Instance::new(SharedMemory::new(65536)); assert_eq!(inst.func3(), 42);",
    );
}

#[test]
fn thread_spawn_works_when_the_module_is_split_across_files() {
    // With one function per file, the spawn method and the child's entry point
    // live in different files of the crate.
    compile_run_split(
        "wasi_threads_split",
        &format!(
            r#"(module {SPAWN_IMPORT}
              (func $twice (param i32) (result i32) local.get 0 i32.const 2 i32.mul)
              (func (export "wasi_thread_start") (param i32 i32)
                i32.const 0 local.get 1 call $twice i32.atomic.store
                i32.const 0 i32.const 1 memory.atomic.notify drop)
              (func (export "run") (result i32)
                i32.const 21 call $spawn drop
                i32.const 0 i32.const 0 i64.const 5000000000
                memory.atomic.wait32 drop
                i32.const 0 i32.atomic.load))"#
        ),
        1,
        "let mut inst = Instance::new(SharedMemory::new(65536)); assert_eq!(inst.func3(), 42);",
    );
}
