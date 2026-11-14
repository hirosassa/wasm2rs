//! Native WASI functions that touch linear memory must also work when the
//! memory is `shared` (defined or imported), where it is a thread-shareable
//! `SharedMemory` rather than a host-borrowed `Vec<u8>`.

use crate::common;

use common::compile_run;

const IMPORT: &str = r#"
      (import "wasi_snapshot_preview1" "fd_write"
        (func $fd_write (param i32 i32 i32 i32) (result i32)))"#;

const BODY: &str = r#"
      (data (i32.const 0) "\10\00\00\00\0c\00\00\00")
      (data (i32.const 16) "hello, wasi\n")
      (func (export "run") (result i32)
        i32.const 1   ;; fd = stdout
        i32.const 0   ;; iovs pointer
        i32.const 1   ;; iovs count
        i32.const 8   ;; nwritten pointer
        call $fd_write
        drop
        i32.const 8
        i32.load))"#;

#[test]
fn fd_write_works_on_a_defined_shared_memory() {
    compile_run(
        "wasi_shared_defined",
        &format!("(module {IMPORT} (memory 1 1 shared) {BODY}"),
        "let mut inst = Instance::new(); assert_eq!(inst.func1(), 12);",
    );
}

#[test]
fn fd_write_works_on_an_imported_shared_memory() {
    compile_run(
        "wasi_shared_imported",
        &format!("(module (import \"env\" \"memory\" (memory 1 1 shared)) {IMPORT} {BODY}"),
        "let mut inst = Instance::new(SharedMemory::new(65536)); assert_eq!(inst.func1(), 12);",
    );
}

#[test]
fn fd_write_gathers_several_iovecs_on_a_shared_memory() {
    // Two iovecs ("hello," and " wasi\n"): each iovec is read with several
    // memory accesses, which must not take the memory lock twice at once.
    compile_run(
        "wasi_shared_two_iovecs",
        r#"(module
            (import "wasi_snapshot_preview1" "fd_write"
              (func $fd_write (param i32 i32 i32 i32) (result i32)))
            (memory 1 1 shared)
            (data (i32.const 0) "\10\00\00\00\06\00\00\00\16\00\00\00\06\00\00\00")
            (data (i32.const 16) "hello, wasi\n")
            (func (export "run") (result i32)
              i32.const 1 i32.const 0 i32.const 2 i32.const 32
              call $fd_write
              drop
              i32.const 32
              i32.load))"#,
        "let mut inst = Instance::new(); assert_eq!(inst.func1(), 12);",
    );
}

#[test]
fn memory_writing_wasi_calls_work_from_many_threads() {
    // `args_sizes_get` stores two u32s into the shared memory. Four threads call
    // it repeatedly while also bumping an atomic counter at address 16; every
    // call must finish (no lock held across calls) and no increment may be lost.
    compile_run(
        "wasi_shared_threads",
        r#"(module
            (import "wasi_snapshot_preview1" "args_sizes_get"
              (func $args_sizes_get (param i32 i32) (result i32)))
            (memory 1 1 shared)
            (func (export "work")
              i32.const 0 i32.const 4
              call $args_sizes_get
              drop
              i32.const 16 i32.const 1 i32.atomic.rmw.add
              drop)
            (func (export "count") (result i32)
              i32.const 16 i32.atomic.load))"#,
        r#"let first = Instance::new();
        let handle = first.shared_memory();
        let mut joins = Vec::new();
        for _ in 0..4 {
            let m = handle.clone();
            joins.push(std::thread::spawn(move || {
                let mut t = Instance::with_memory(m);
                for _ in 0..500 { t.func1(); }
            }));
        }
        for j in joins { j.join().unwrap(); }
        let mut reader = Instance::with_memory(handle);
        assert_eq!(reader.func2(), 2000);"#,
    );
}

#[test]
fn every_native_wasi_function_compiles_on_a_shared_memory() {
    // (import name, parameter types); every function returns an i32 errno except
    // `proc_exit`.
    // Every recognised function gets a shared-memory body; the whole set must
    // compile together under `-D warnings`.
    let funcs: &[(&str, &[&str])] = &[
        ("proc_exit", &["i32"]),
        ("fd_write", &["i32", "i32", "i32", "i32"]),
        ("fd_read", &["i32", "i32", "i32", "i32"]),
        ("fd_close", &["i32"]),
        ("fd_seek", &["i32", "i64", "i32", "i32"]),
        ("fd_fdstat_get", &["i32", "i32"]),
        ("args_sizes_get", &["i32", "i32"]),
        ("args_get", &["i32", "i32"]),
        ("environ_sizes_get", &["i32", "i32"]),
        ("environ_get", &["i32", "i32"]),
        ("clock_time_get", &["i32", "i64", "i32"]),
        ("random_get", &["i32", "i32"]),
        (
            "path_open",
            &[
                "i32", "i32", "i32", "i32", "i32", "i64", "i64", "i32", "i32",
            ],
        ),
        ("fd_prestat_get", &["i32", "i32"]),
        ("fd_prestat_dir_name", &["i32", "i32", "i32"]),
        ("fd_filestat_get", &["i32", "i32"]),
        ("sched_yield", &[]),
        ("clock_res_get", &["i32", "i32"]),
        ("fd_pread", &["i32", "i32", "i32", "i64", "i32"]),
        ("fd_pwrite", &["i32", "i32", "i32", "i64", "i32"]),
        ("path_filestat_get", &["i32", "i32", "i32", "i32", "i32"]),
        ("path_create_directory", &["i32", "i32", "i32"]),
        ("path_remove_directory", &["i32", "i32", "i32"]),
        ("path_unlink_file", &["i32", "i32", "i32"]),
        ("path_rename", &["i32", "i32", "i32", "i32", "i32", "i32"]),
        ("path_symlink", &["i32", "i32", "i32", "i32", "i32"]),
        (
            "path_link",
            &["i32", "i32", "i32", "i32", "i32", "i32", "i32"],
        ),
        ("fd_readdir", &["i32", "i32", "i32", "i64", "i32"]),
        ("fd_tell", &["i32", "i32"]),
        ("fd_sync", &["i32"]),
        ("fd_datasync", &["i32"]),
        ("fd_advise", &["i32", "i64", "i64", "i32"]),
        ("fd_allocate", &["i32", "i64", "i64"]),
        ("fd_filestat_set_size", &["i32", "i64"]),
        ("fd_filestat_set_times", &["i32", "i64", "i64", "i32"]),
        (
            "path_filestat_set_times",
            &["i32", "i32", "i32", "i32", "i64", "i64", "i32"],
        ),
        ("path_readlink", &["i32", "i32", "i32", "i32", "i32", "i32"]),
        ("fd_fdstat_set_flags", &["i32", "i32"]),
        ("fd_fdstat_set_rights", &["i32", "i64", "i64"]),
        ("fd_renumber", &["i32", "i32"]),
        ("poll_oneoff", &["i32", "i32", "i32", "i32"]),
        ("sock_accept", &["i32", "i32", "i32"]),
        ("sock_recv", &["i32", "i32", "i32", "i32", "i32", "i32"]),
        ("sock_send", &["i32", "i32", "i32", "i32", "i32"]),
        ("sock_shutdown", &["i32", "i32"]),
        ("proc_raise", &["i32"]),
    ];
    let mut imports = String::new();
    for (name, params) in funcs {
        let params = params.join(" ");
        let result = if *name == "proc_exit" {
            ""
        } else {
            "(result i32)"
        };
        imports.push_str(&format!(
            "(import \"wasi_snapshot_preview1\" \"{name}\" (func (param {params}) {result}))\n"
        ));
    }
    compile_run(
        "wasi_shared_every_function",
        &format!("(module {imports} (memory 1 1 shared))"),
        "let _inst = Instance::new();",
    );
}

#[test]
fn random_get_and_clock_time_get_write_into_a_shared_memory() {
    compile_run(
        "wasi_shared_random_clock",
        r#"(module
            (import "wasi_snapshot_preview1" "random_get"
              (func $random_get (param i32 i32) (result i32)))
            (import "wasi_snapshot_preview1" "clock_time_get"
              (func $clock_time_get (param i32 i64 i32) (result i32)))
            (memory 1 1 shared)
            (func (export "rand") (result i32)
              i32.const 0 i32.const 64 call $random_get)
            (func (export "clock") (result i32)
              i32.const 0 i64.const 0 i32.const 128 call $clock_time_get)
            (func (export "clock_value") (result i64)
              i32.const 128 i64.load))"#,
        r#"let mut inst = Instance::new();
        assert_eq!(inst.func2(), 0);
        assert_eq!(inst.func3(), 0);
        assert_ne!(inst.func4(), 0, "the clock value was stored");
"#,
    );
}
