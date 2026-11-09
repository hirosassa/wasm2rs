//! The `wasi:filesystem/types` calls that complete the typed host's descriptor API:
//! `get-type`, `read` and `write` (positional, no stream), `advise`, `is-same-object` and
//! `filesystem-error-code`. Each export of `CORE` returns what the test reads: the value, or
//! an error as `100 + the error-code` (`-(1 + the error-code)` where the value is a length).

use crate::wasip2_typed_fs::{CLOCKS, WORLD, fresh_root};
use crate::wasip2_typed_host::{assert_succeeded, component_from_wit, run_program};

const IO: &str = r#"
package wasi:io@0.2.0;
interface error {
  resource error;
}
interface poll {
  resource pollable;
}
interface streams {
  use error.{error};
  use poll.{pollable};
  variant stream-error { last-operation-failed(error), closed }
  resource input-stream {
    read: func(len: u64) -> result<list<u8>, stream-error>;
  }
}
"#;

const FILESYSTEM: &str = r#"
package wasi:filesystem@0.2.0;
interface types {
  use wasi:io/streams@0.2.0.{input-stream, error};
  use wasi:clocks/wall-clock@0.2.0.{datetime};
  type filesize = u64;
  enum descriptor-type {
    unknown, block-device, character-device, directory, fifo, symbolic-link, regular-file, socket
  }
  flags descriptor-flags {
    read, write, file-integrity-sync, data-integrity-sync, requested-write-sync, mutate-directory
  }
  flags path-flags { symlink-follow }
  flags open-flags { create, directory, exclusive, truncate }
  enum advice { normal, sequential, random, will-need, dont-need, no-reuse }
  enum error-code {
    access, would-block, already, bad-descriptor, busy, deadlock, quota, exist, file-too-large,
    illegal-byte-sequence, in-progress, interrupted, invalid, io, is-directory, loop,
    too-many-links, message-size, name-too-long, no-device, no-entry, no-lock,
    insufficient-memory, insufficient-space, not-directory, not-empty, not-recoverable,
    unsupported, no-tty, no-such-device, overflow, not-permitted, pipe, read-only,
    invalid-seek, text-file-busy, cross-device
  }
  resource descriptor {
    read-via-stream: func(offset: filesize) -> result<input-stream, error-code>;
    open-at: func(path-flags: path-flags, path: string, open-flags: open-flags, %flags: descriptor-flags) -> result<descriptor, error-code>;
    unlink-file-at: func(path: string) -> result<_, error-code>;
    create-directory-at: func(path: string) -> result<_, error-code>;
    get-type: func() -> result<descriptor-type, error-code>;
    read: func(length: filesize, offset: filesize) -> result<tuple<list<u8>, bool>, error-code>;
    write: func(buffer: list<u8>, offset: filesize) -> result<filesize, error-code>;
    advise: func(offset: filesize, length: filesize, advice: advice) -> result<_, error-code>;
    is-same-object: func(other: borrow<descriptor>) -> bool;
  }
  filesystem-error-code: func(err: borrow<error>) -> option<error-code>;
}
interface preopens {
  use types.{descriptor};
  get-directories: func() -> list<tuple<descriptor, string>>;
}
"#;

const CORE: &str = r#"
(module
  (import "wasi:io/error@0.2.0" "[resource-drop]error" (func $drop_error (param i32)))
  (import "wasi:io/poll@0.2.0" "[resource-drop]pollable" (func (param i32)))
  (import "wasi:io/streams@0.2.0" "[resource-drop]input-stream" (func (param i32)))
  (import "wasi:io/streams@0.2.0" "[method]input-stream.read" (func $in_read (param i32 i64 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.read-via-stream" (func $rvs (param i32 i64 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.open-at" (func $open (param i32 i32 i32 i32 i32 i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.unlink-file-at" (func $unlink (param i32 i32 i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.create-directory-at" (func $mkdir (param i32 i32 i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.get-type" (func $get_type (param i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.read" (func $pread (param i32 i64 i64 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.write" (func $pwrite (param i32 i32 i32 i64 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.advise" (func $advise (param i32 i64 i64 i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.is-same-object" (func $same (param i32 i32) (result i32)))
  (import "wasi:filesystem/types@0.2.0" "filesystem-error-code" (func $err_code (param i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[resource-drop]descriptor" (func $drop_fd (param i32)))
  (import "wasi:filesystem/preopens@0.2.0" "get-directories" (func $dirs (param i32)))
  (memory (export "memory") 1)
  (global $brk (mut i32) (i32.const 2048))
  (global $last (mut i32) (i32.const 0))
  (global $failed (mut i32) (i32.const 0))
  (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32) (local $p i32)
    (local.set $p (i32.and (i32.add (global.get $brk) (i32.sub (local.get 2) (i32.const 1)))
                           (i32.sub (i32.const 0) (local.get 2))))
    (global.set $brk (i32.add (local.get $p) (local.get 3)))
    (local.get $p))
  (data (i32.const 400) "f.txt")
  (data (i32.const 416) "g.txt")
  (data (i32.const 432) "d")
  (data (i32.const 448) "ab")

  ;; a `result<_, error-code>` at `at`: 0, or 100 + the error code
  (func $status (param $at i32) (result i32)
    (if (result i32) (i32.load8_u (local.get $at))
      (then (i32.add (i32.const 100) (i32.load8_u (i32.add (local.get $at) (i32.const 1)))))
      (else (i32.const 0))))

  ;; the handle of the preopen is 0
  ;; open-at under descriptor `dir`: 0 (the handle is `fs_last`) or 100 + the error code
  (func (export "fs_open") (param $dir i32) (param $p i32) (param $n i32) (param $o i32) (param $d i32) (result i32)
    (call $open (local.get $dir) (i32.const 1) (local.get $p) (local.get $n)
                (local.get $o) (local.get $d) (i32.const 200))
    (if (result i32) (i32.load8_u (i32.const 200))
      (then (i32.add (i32.const 100) (i32.load8_u (i32.const 204))))
      (else (global.set $last (i32.load (i32.const 204))) (i32.const 0))))
  (func (export "fs_last") (result i32) (global.get $last))
  (func (export "fs_unlink") (param $dir i32) (param $p i32) (param $n i32) (result i32)
    (call $unlink (local.get $dir) (local.get $p) (local.get $n) (i32.const 220))
    (call $status (i32.const 220)))
  (func (export "fs_mkdir") (param $dir i32) (param $p i32) (param $n i32) (result i32)
    (call $mkdir (local.get $dir) (local.get $p) (local.get $n) (i32.const 220))
    (call $status (i32.const 220)))
  (func (export "fs_drop") (param i32) (call $drop_fd (local.get 0)))

  ;; get-type: the descriptor-type (0 to 7), or 100 + the error code
  (func (export "fs_get_type") (param $h i32) (result i32)
    (call $get_type (local.get $h) (i32.const 240))
    (if (result i32) (i32.load8_u (i32.const 240))
      (then (i32.add (i32.const 100) (i32.load8_u (i32.const 241))))
      (else (i32.load8_u (i32.const 241)))))

  ;; read(length, offset): the length read, or -(1 + the error code); `rd_eof` and `rd_byte` show the rest
  (func (export "fs_pread") (param $h i32) (param $len i64) (param $off i64) (result i32)
    (call $pread (local.get $h) (local.get $len) (local.get $off) (i32.const 260))
    (if (result i32) (i32.load8_u (i32.const 260))
      (then (i32.sub (i32.const -1) (i32.load8_u (i32.const 264))))
      (else (i32.load (i32.const 268)))))
  (func (export "rd_eof") (result i32) (i32.load8_u (i32.const 272)))
  (func (export "rd_byte") (param $i i32) (result i32)
    (i32.load8_u (i32.add (i32.load (i32.const 264)) (local.get $i))))

  ;; write(buffer at (p, n), offset): the length written, or -(1 + the error code)
  (func (export "fs_pwrite") (param $h i32) (param $p i32) (param $n i32) (param $off i64) (result i64)
    (call $pwrite (local.get $h) (local.get $p) (local.get $n) (local.get $off) (i32.const 280))
    (if (result i64) (i32.load8_u (i32.const 280))
      (then (i64.sub (i64.const -1) (i64.load8_u (i32.const 288))))
      (else (i64.load (i32.const 288)))))

  (func (export "fs_advise") (param $h i32) (param $off i64) (param $len i64) (param $advice i32) (result i32)
    (call $advise (local.get $h) (local.get $off) (local.get $len) (local.get $advice) (i32.const 300))
    (call $status (i32.const 300)))

  (func (export "fs_same") (param $a i32) (param $b i32) (result i32)
    (call $same (local.get $a) (local.get $b)))

  ;; read-via-stream: the stream handle, or -1
  (func (export "fs_stream") (param $h i32) (param $offset i64) (result i32)
    (call $rvs (local.get $h) (local.get $offset) (i32.const 320))
    (if (result i32) (i32.load8_u (i32.const 320))
      (then (i32.const -1))
      (else (i32.load (i32.const 324)))))
  ;; input-stream.read: the length, -2 for `closed`, -3 for `last-operation-failed` (its error
  ;; handle is `last_failed`)
  (func (export "in_read") (param $s i32) (param $len i64) (result i32)
    (call $in_read (local.get $s) (local.get $len) (i32.const 340))
    (if (result i32) (i32.load8_u (i32.const 340))
      (then
        (if (result i32) (i32.load8_u (i32.const 344))
          (then (i32.const -2))
          (else (global.set $failed (i32.load (i32.const 348))) (i32.const -3))))
      (else (i32.load (i32.const 348)))))
  (func (export "last_failed") (result i32) (global.get $failed))
  (func (export "drop_error") (param i32) (call $drop_error (local.get 0)))

  ;; filesystem-error-code: the error-code, or -1 for none
  (func (export "fs_error_code") (param $e i32) (result i32)
    (call $err_code (local.get $e) (i32.const 360))
    (if (result i32) (i32.load8_u (i32.const 360))
      (then (i32.load8_u (i32.const 361)))
      (else (i32.const -1)))))
"#;

// the strings of CORE
const F: &str = "400, 5";
const G: &str = "416, 5";
const D: &str = "432, 1";
const AB: &str = "448, 2";

// open-flags: create 1, directory 2; descriptor-flags: read 1, write 2
// descriptor-type: directory 3, regular-file 6
// error codes: bad-descriptor 3, io 13, is-directory 14, read-only 33

/// Runs `body` with `inst` on a host whose preopen is `root`, writable or not.
fn run(root: &std::path::Path, writable: bool, body: &str) -> std::process::Output {
    let preopen = if writable {
        "with_writable_preopen"
    } else {
        "with_preopen"
    };
    run_program(
        &component_from_wit(&[CLOCKS, IO, FILESYSTEM], WORLD, "w"),
        CORE,
        &format!(
            "WasiHost::new(Vec::new()).{preopen}(std::path::PathBuf::from({:?}))",
            root.to_str().unwrap()
        ),
        body,
    )
}

#[test]
fn get_type_tells_a_directory_from_a_regular_file() {
    let root = fresh_root("gaps_get_type");
    std::fs::write(root.join("f.txt"), "x").unwrap();
    std::fs::create_dir(root.join("d")).unwrap();
    let body = format!(
        "assert_eq!(inst.fs_get_type(0), 3);
         assert_eq!(inst.fs_open(0, {F}, 0, 1), 0);
         let last = inst.fs_last();
         assert_eq!(inst.fs_get_type(last), 6);
         assert_eq!(inst.fs_open(0, {D}, 2, 1), 0);
         let last = inst.fs_last();
         assert_eq!(inst.fs_get_type(last), 3);"
    );
    assert_succeeded(&run(&root, false, &body));
}

#[test]
fn read_returns_the_bytes_at_an_offset_and_whether_the_end_was_reached() {
    let root = fresh_root("gaps_read");
    std::fs::write(root.join("f.txt"), "0123456789").unwrap();
    std::fs::create_dir(root.join("d")).unwrap();
    let body = format!(
        "let bytes = |inst: &mut Instance<Glue<WasiHost>>, n: i32| -> Vec<u8> {{
             (0..n).map(|i| inst.rd_byte(i) as u8).collect()
         }};
         assert_eq!(inst.fs_open(0, {F}, 0, 1), 0);
         let f = inst.fs_last();
         // fewer bytes than the file has: not the end
         assert_eq!(inst.fs_pread(f, 4, 2), 4);
         assert_eq!(bytes(&mut inst, 4), b\"2345\");
         assert_eq!(inst.rd_eof(), 0);
         // asking for more than is left returns what is left, and the end
         assert_eq!(inst.fs_pread(f, 100, 6), 4);
         assert_eq!(bytes(&mut inst, 4), b\"6789\");
         assert_eq!(inst.rd_eof(), 1);
         // at or past the end: nothing, and the end
         assert_eq!(inst.fs_pread(f, 4, 10), 0);
         assert_eq!(inst.rd_eof(), 1);
         assert_eq!(inst.fs_pread(f, 4, 1000), 0);
         assert_eq!(inst.rd_eof(), 1);
         // a zero length reads nothing and is not the end
         assert_eq!(inst.fs_pread(f, 0, 3), 0);
         assert_eq!(inst.rd_eof(), 0);
         // the calls do not share a position: the first range reads the same again
         assert_eq!(inst.fs_pread(f, 4, 2), 4);
         assert_eq!(bytes(&mut inst, 4), b\"2345\");
         // a directory is is-directory (-(1 + 14)); a descriptor for writing only is
         // bad-descriptor (-(1 + 3))
         assert_eq!(inst.fs_pread(0, 4, 0), -15);
         assert_eq!(inst.fs_open(0, {F}, 0, 2), 0);
         let last = inst.fs_last();
         assert_eq!(inst.fs_pread(last, 4, 0), -4);"
    );
    assert_succeeded(&run(&root, true, &body));
}

#[test]
fn read_at_an_offset_no_file_has_is_an_error_not_the_end() {
    let root = fresh_root("gaps_read_bad_offset");
    std::fs::write(root.join("f.txt"), "0123456789").unwrap();
    // the seek to an offset past `i64::MAX` fails with EINVAL on every platform
    let body = format!(
        "assert_eq!(inst.fs_open(0, {F}, 0, 1), 0);
         let last = inst.fs_last();
         assert!(inst.fs_pread(last, 4, i64::MIN) < 0);"
    );
    assert_succeeded(&run(&root, false, &body));
}

#[test]
fn write_puts_the_bytes_at_an_offset_and_extends_the_file_with_zeros() {
    let root = fresh_root("gaps_write");
    std::fs::write(root.join("f.txt"), "0123456789").unwrap();
    std::fs::create_dir(root.join("d")).unwrap();
    let body = format!(
        "assert_eq!(inst.fs_open(0, {F}, 0, 3), 0);
         let f = inst.fs_last();
         assert_eq!(inst.fs_pwrite(f, {AB}, 3), 2);
         // past the end of the file: the gap is zeros
         assert_eq!(inst.fs_pwrite(f, {AB}, 12), 2);
         // a descriptor for reading only is bad-descriptor (-(1 + 3)), a directory is
         // is-directory (-(1 + 14))
         assert_eq!(inst.fs_open(0, {F}, 0, 1), 0);
         let last = inst.fs_last();
         assert_eq!(inst.fs_pwrite(last, {AB}, 0), -4);
         assert_eq!(inst.fs_pwrite(0, {AB}, 0), -15);"
    );
    assert_succeeded(&run(&root, true, &body));
    assert_eq!(
        std::fs::read(root.join("f.txt")).unwrap(),
        b"012ab56789\0\0ab"
    );
}

#[test]
fn write_needs_a_writable_preopen() {
    let root = fresh_root("gaps_write_read_only");
    std::fs::write(root.join("f.txt"), "0123456789").unwrap();
    // read-only (-(1 + 33)); the preopen does not even open a file for writing
    let body = format!(
        "assert_eq!(inst.fs_open(0, {F}, 0, 3), 100 + 33);
         assert_eq!(inst.fs_open(0, {F}, 0, 1), 0);
         let last = inst.fs_last();
         assert_eq!(inst.fs_pwrite(last, {AB}, 0), -34);"
    );
    assert_succeeded(&run(&root, false, &body));
    assert_eq!(std::fs::read(root.join("f.txt")).unwrap(), b"0123456789");
}

#[test]
fn advise_is_a_hint_that_changes_nothing_but_needs_a_file() {
    let root = fresh_root("gaps_advise");
    std::fs::write(root.join("f.txt"), "0123456789").unwrap();
    let body = format!(
        "assert_eq!(inst.fs_open(0, {F}, 0, 1), 0);
         let f = inst.fs_last();
         // every advice is accepted, whatever the range (even past the end)
         for advice in 0..6 {{
             assert_eq!(inst.fs_advise(f, 0, 10, advice), 0);
             assert_eq!(inst.fs_advise(f, 1 << 40, u64::MAX as i64, advice), 0);
         }}
         // a directory is is-directory (14)
         assert_eq!(inst.fs_advise(0, 0, 0, 0), 100 + 14);
         // the file is unchanged
         assert_eq!(inst.fs_pread(f, 100, 0), 10);"
    );
    assert_succeeded(&run(&root, false, &body));
    assert_eq!(std::fs::read(root.join("f.txt")).unwrap(), b"0123456789");
}

#[test]
fn is_same_object_compares_what_the_descriptors_name() {
    let root = fresh_root("gaps_same");
    std::fs::write(root.join("f.txt"), "x").unwrap();
    std::fs::write(root.join("g.txt"), "x").unwrap();
    std::fs::create_dir(root.join("d")).unwrap();
    let body = format!(
        "assert_eq!(inst.fs_open(0, {F}, 0, 1), 0);
         let f1 = inst.fs_last();
         assert_eq!(inst.fs_open(0, {F}, 0, 1), 0);
         let f2 = inst.fs_last();
         assert_eq!(inst.fs_open(0, {G}, 0, 1), 0);
         let g = inst.fs_last();
         assert_eq!(inst.fs_open(0, {D}, 2, 1), 0);
         let d = inst.fs_last();
         // a descriptor is the same object as itself, and two opens of one file are
         assert_eq!(inst.fs_same(f1, f1), 1);
         assert_eq!(inst.fs_same(f1, f2), 1);
         assert_eq!(inst.fs_same(0, 0), 1);
         // another file, and a file and a directory, are not
         assert_eq!(inst.fs_same(f1, g), 0);
         assert_eq!(inst.fs_same(f1, d), 0);
         assert_eq!(inst.fs_same(0, d), 0);
         assert_eq!(inst.fs_same(d, f1), 0);"
    );
    assert_succeeded(&run(&root, false, &body));
}

// needs POSIX semantics: a file unlinked while open stays reachable, so its inode is taken
#[cfg(unix)]
#[test]
fn is_same_object_tells_a_file_from_the_one_that_took_its_path() {
    let root = fresh_root("gaps_same_replaced");
    std::fs::write(root.join("f.txt"), "x").unwrap();
    let body = format!(
        "assert_eq!(inst.fs_open(0, {F}, 0, 1), 0);
         let old = inst.fs_last();
         assert_eq!(inst.fs_unlink(0, {F}), 0);
         assert_eq!(inst.fs_open(0, {F}, 1, 3), 0);
         let new = inst.fs_last();
         assert_eq!(inst.fs_same(old, new), 0);"
    );
    assert_succeeded(&run(&root, true, &body));
}

#[test]
fn filesystem_error_code_names_the_filesystem_failure_behind_an_error() {
    let root = fresh_root("gaps_error_code");
    std::fs::write(root.join("f.txt"), "0123456789").unwrap();
    // a read at an offset past `i64::MAX` fails in the host's seek with EINVAL, which the host reports as `io` (13)
    let body = format!(
        "assert_eq!(inst.fs_open(0, {F}, 0, 1), 0);
         let f = inst.fs_last();
         let s = inst.fs_stream(f, i64::MIN);
         assert_eq!(inst.in_read(s, 4), -3);
         let error = inst.last_failed();
         assert_eq!(inst.fs_error_code(error), 13);
         // asking again gives the same answer: the error is not consumed
         assert_eq!(inst.fs_error_code(error), 13);
         // an error the host does not hold has no filesystem code
         inst.drop_error(error);
         assert_eq!(inst.fs_error_code(error), -1);"
    );
    assert_succeeded(&run(&root, false, &body));
}
