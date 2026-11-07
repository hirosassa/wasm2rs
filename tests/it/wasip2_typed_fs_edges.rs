//! Edge cases of the typed host's filesystem files and listings: the four read calls on a
//! file stream (`read`, `blocking-read`, `skip`, `blocking-skip`), `metadata-hash` of
//! distinct files, `read-directory` boundaries and `open-at` errors. The imports are
//! declared in WIT text; error codes are read as `100 + the error-code byte`.

use std::path::Path;

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
    blocking-read: func(len: u64) -> result<list<u8>, stream-error>;
    skip: func(len: u64) -> result<u64, stream-error>;
    blocking-skip: func(len: u64) -> result<u64, stream-error>;
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
  record directory-entry { %type: descriptor-type, name: string }
  record metadata-hash-value { lower: u64, upper: u64 }
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
    read-directory: func() -> result<directory-entry-stream, error-code>;
    open-at: func(path-flags: path-flags, path: string, open-flags: open-flags, %flags: descriptor-flags) -> result<descriptor, error-code>;
    metadata-hash: func() -> result<metadata-hash-value, error-code>;
    metadata-hash-at: func(path-flags: path-flags, path: string) -> result<metadata-hash-value, error-code>;
  }
  resource directory-entry-stream {
    read-directory-entry: func() -> result<option<directory-entry>, error-code>;
  }
}
interface preopens {
  use types.{descriptor};
  get-directories: func() -> list<tuple<descriptor, string>>;
}
"#;

const CORE: &str = r#"
(module
  (import "wasi:io/error@0.2.0" "[resource-drop]error" (func (param i32)))
  (import "wasi:io/poll@0.2.0" "[resource-drop]pollable" (func (param i32)))
  (import "wasi:io/streams@0.2.0" "[method]input-stream.read" (func $read (param i32 i64 i32)))
  (import "wasi:io/streams@0.2.0" "[method]input-stream.blocking-read" (func $bread (param i32 i64 i32)))
  (import "wasi:io/streams@0.2.0" "[method]input-stream.skip" (func $skip (param i32 i64 i32)))
  (import "wasi:io/streams@0.2.0" "[method]input-stream.blocking-skip" (func $bskip (param i32 i64 i32)))
  (import "wasi:io/streams@0.2.0" "[resource-drop]input-stream" (func $drop_stream (param i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.read-via-stream" (func $rvs (param i32 i64 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.read-directory" (func $readdir (param i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.open-at" (func $open (param i32 i32 i32 i32 i32 i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.metadata-hash" (func $hash (param i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.metadata-hash-at" (func $hash_at (param i32 i32 i32 i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]directory-entry-stream.read-directory-entry" (func $entry (param i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[resource-drop]descriptor" (func $drop_fd (param i32)))
  (import "wasi:filesystem/types@0.2.0" "[resource-drop]directory-entry-stream" (func $drop_ls (param i32)))
  (import "wasi:filesystem/preopens@0.2.0" "get-directories" (func $dirs (param i32)))
  (memory (export "memory") 1)
  (global $brk (mut i32) (i32.const 1024))
  (global $last (mut i32) (i32.const 0))
  (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32) (local $p i32)
    (local.set $p (i32.and (i32.add (global.get $brk) (i32.sub (local.get 2) (i32.const 1)))
                           (i32.sub (i32.const 0) (local.get 2))))
    (global.set $brk (i32.add (local.get $p) (local.get 3)))
    (local.get $p))
  (data (i32.const 300) "a.txt")
  (data (i32.const 320) "b.txt")
  (data (i32.const 340) "d")
  (data (i32.const 360) "a.txt/x")
  (data (i32.const 380) "empty")

  (func $root (result i32)
    (call $dirs (i32.const 100))
    (i32.load (i32.load (i32.const 100))))
  (func $code (param $at i32) (result i32)
    (i32.add (i32.const 100) (i32.load8_u (local.get $at))))

  ;; open-at from the preopen: 0 (the handle is `fs_last`) or 100 + the error code
  (func (export "fs_open") (param $p i32) (param $n i32) (param $o i32) (param $d i32) (result i32)
    (call $open (call $root) (i32.const 1) (local.get $p) (local.get $n)
                (local.get $o) (local.get $d) (i32.const 200))
    (if (result i32) (i32.load8_u (i32.const 200))
      (then (call $code (i32.const 204)))
      (else (global.set $last (i32.load (i32.const 204))) (i32.const 0))))
  (func (export "fs_last") (result i32) (global.get $last))

  ;; read-via-stream: the stream handle, or -1
  (func (export "fs_stream") (param $h i32) (param $offset i64) (result i32)
    (call $rvs (local.get $h) (local.get $offset) (i32.const 240))
    (if (result i32) (i32.load8_u (i32.const 240))
      (then (i32.const -1))
      (else (i32.load (i32.const 244)))))

  ;; read / blocking-read: -1 at the end (`closed`), else the length | the first byte << 32
  (func $got (result i64)
    (if (result i64) (i32.load8_u (i32.const 260))
      (then (i64.const -1))
      (else
        (i64.or
          (i64.extend_i32_u (i32.load (i32.const 268)))
          (if (result i64) (i32.load (i32.const 268))
            (then (i64.shl (i64.extend_i32_u (i32.load8_u (i32.load (i32.const 264)))) (i64.const 32)))
            (else (i64.const 0)))))))
  (func (export "fs_read") (param $s i32) (param $len i64) (result i64)
    (call $read (local.get $s) (local.get $len) (i32.const 260))
    (call $got))
  (func (export "fs_bread") (param $s i32) (param $len i64) (result i64)
    (call $bread (local.get $s) (local.get $len) (i32.const 260))
    (call $got))
  ;; skip / blocking-skip: the count, or -1 at the end
  (func (export "fs_skip") (param $s i32) (param $len i64) (result i64)
    (call $skip (local.get $s) (local.get $len) (i32.const 280))
    (if (result i64) (i32.load8_u (i32.const 280)) (then (i64.const -1)) (else (i64.load (i32.const 288)))))
  (func (export "fs_bskip") (param $s i32) (param $len i64) (result i64)
    (call $bskip (local.get $s) (local.get $len) (i32.const 280))
    (if (result i64) (i32.load8_u (i32.const 280)) (then (i64.const -1)) (else (i64.load (i32.const 288)))))

  ;; the low half of the hash of a descriptor / of a path under the preopen; 0 on error
  (func (export "fs_hash") (param $h i32) (result i64)
    (call $hash (local.get $h) (i32.const 720))
    (if (result i64) (i32.load8_u (i32.const 720)) (then (i64.const 0)) (else (i64.load (i32.const 728)))))
  (func (export "fs_hash_at") (param $p i32) (param $n i32) (result i64)
    (call $hash_at (call $root) (i32.const 1) (local.get $p) (local.get $n) (i32.const 720))
    (if (result i64) (i32.load8_u (i32.const 720)) (then (i64.const 0)) (else (i64.load (i32.const 728)))))
  (func (export "fs_hash_at_code") (param $p i32) (param $n i32) (result i32)
    (call $hash_at (call $root) (i32.const 1) (local.get $p) (local.get $n) (i32.const 720))
    (if (result i32) (i32.load8_u (i32.const 720)) (then (call $code (i32.const 728))) (else (i32.const 0))))

  ;; read-directory: the listing's handle, or 100 + the error code
  (func (export "fs_listing") (param $h i32) (result i32)
    (call $readdir (local.get $h) (i32.const 800))
    (if (result i32) (i32.load8_u (i32.const 800))
      (then (call $code (i32.const 804)))
      (else (i32.load (i32.const 804)))))
  ;; the next entry: 100 + the error code, -1 for the end of the listing, else the name's length | its first byte << 8
  (func (export "fs_next") (param $s i32) (result i32)
    (call $entry (local.get $s) (i32.const 820))
    (if (result i32) (i32.load8_u (i32.const 820))
      (then (call $code (i32.const 824)))
      (else
        (if (result i32) (i32.load8_u (i32.const 824))
          (then (i32.or (i32.load (i32.const 836))
                        (i32.shl (i32.load8_u (i32.load (i32.const 832))) (i32.const 8))))
          (else (i32.const -1))))))

  (func (export "fs_drop") (param i32) (call $drop_fd (local.get 0)))
  (func (export "fs_drop_stream") (param i32) (call $drop_stream (local.get 0)))
  (func (export "fs_drop_listing") (param i32) (call $drop_ls (local.get 0))))
"#;

const A: &str = "300, 5";
const B: &str = "320, 5";
const D: &str = "340, 1";
const A_THROUGH: &str = "360, 7";
const EMPTY: &str = "380, 5";

fn run(root: &Path, body: &str) -> std::process::Output {
    run_program(
        &component_from_wit(&[CLOCKS, IO, FILESYSTEM], WORLD, "w"),
        CORE,
        &format!(
            "TypedHost::new(Vec::new()).with_preopen(std::path::PathBuf::from({:?}))",
            root.to_str().unwrap()
        ),
        body,
    )
}

#[test]
fn file_reads_and_skips_share_one_position_and_end_as_closed() {
    let root = fresh_root("edges_stream");
    std::fs::write(root.join("a.txt"), "0123456789").unwrap();
    let body = format!(
        "assert_eq!(inst.fs_open({A}, 0, 1), 0);
         let f = inst.fs_last();
         let s = inst.fs_stream(f, 0);
         // skip advances the position by what it discarded: the next byte is '3' (51)
         assert_eq!(inst.fs_skip(s, 3), 3);
         assert_eq!(inst.fs_read(s, 2), 2 | 51 << 32);
         // blocking-skip does the same, and blocking-read follows it: '6' (54)
         assert_eq!(inst.fs_bskip(s, 1), 1);
         assert_eq!(inst.fs_bread(s, 1), 1 | 54 << 32);
         // asking for more than is left gives what is left ('7','8','9')
         assert_eq!(inst.fs_skip(s, 100), 3);
         // at the end every call reports `closed`
         assert_eq!(inst.fs_read(s, 1), -1);
         assert_eq!(inst.fs_bread(s, 1), -1);
         assert_eq!(inst.fs_skip(s, 1), -1);
         assert_eq!(inst.fs_bskip(s, 1), -1);
         // a stream at an offset starts there; an offset at the end of the file is closed at
         // once, and so is one beyond it
         let t = inst.fs_stream(f, 8);
         assert_eq!(inst.fs_bread(t, 10), 2 | 56 << 32);
         let u = inst.fs_stream(f, 10);
         assert_eq!(inst.fs_bread(u, 1), -1);
         let v = inst.fs_stream(f, 11);
         assert_eq!(inst.fs_bread(v, 1), -1);"
    );
    assert_succeeded(&run(&root, &body));
}

#[test]
fn blocking_read_and_both_skips_move_at_most_4096_bytes_of_a_file_at_once() {
    let root = fresh_root("edges_chunk");
    let mut big = vec![b'x'; 10_000];
    big[4096] = b'y';
    std::fs::write(root.join("a.txt"), &big).unwrap();
    let body = format!(
        "assert_eq!(inst.fs_open({A}, 0, 1), 0);
         let f = inst.fs_last();
         // blocking-read: 4096, then the next chunk starts at the 'y' (121)
         let s = inst.fs_stream(f, 0);
         assert_eq!(inst.fs_bread(s, -1), 4096 | 120 << 32);
         assert_eq!(inst.fs_bread(s, -1), 4096 | 121 << 32);
         assert_eq!(inst.fs_bread(s, -1), 1808 | 120 << 32);
         assert_eq!(inst.fs_bread(s, -1), -1);
         // skip and blocking-skip: 4096 per call as well, 10000 = 4096 + 4096 + 1808
         let t = inst.fs_stream(f, 0);
         assert_eq!(inst.fs_skip(t, -1), 4096);
         assert_eq!(inst.fs_bskip(t, 5000), 4096);
         assert_eq!(inst.fs_skip(t, -1), 1808);
         assert_eq!(inst.fs_bskip(t, -1), -1);"
    );
    assert_succeeded(&run(&root, &body));
}

#[test]
fn metadata_hash_tells_files_apart_and_agrees_between_the_two_calls() {
    let root = fresh_root("edges_hash");
    // same size and content: only the identity differs
    std::fs::write(root.join("a.txt"), "same").unwrap();
    std::fs::write(root.join("b.txt"), "same").unwrap();
    std::fs::create_dir(root.join("d")).unwrap();
    let body = format!(
        "let a = inst.fs_hash_at({A});
         let b = inst.fs_hash_at({B});
         let d = inst.fs_hash_at({D});
         assert!(a != 0 && b != 0 && d != 0);
         assert_ne!(a, b);
         assert_ne!(a, d);
         assert_ne!(b, d);
         // stable across calls, and the same through a descriptor of the file
         assert_eq!(inst.fs_hash_at({A}), a);
         assert_eq!(inst.fs_open({A}, 0, 1), 0);
         let fa = inst.fs_last();
         assert_eq!(inst.fs_hash(fa), a);
         assert_eq!(inst.fs_open({B}, 0, 1), 0);
         let fb = inst.fs_last();
         assert_eq!(inst.fs_hash(fb), b);
         assert_ne!(inst.fs_hash(fa), inst.fs_hash(fb));
         // the preopen is the directory `.`; a file does not hash like it
         let preopen = inst.fs_hash(0);
         assert_ne!(preopen, 0);
         assert_ne!(preopen, a);
         // a missing path (`empty` does not exist in this root) is no-entry 20, a file in the middle of the path not-directory 24
         assert_eq!(inst.fs_hash_at_code({EMPTY}), 100 + 20);
         assert_eq!(inst.fs_hash_at_code({A_THROUGH}), 100 + 24);"
    );
    assert_succeeded(&run(&root, &body));
}

#[test]
fn read_directory_of_an_empty_directory_ends_at_once_and_stays_ended() {
    let root = fresh_root("edges_empty");
    std::fs::create_dir(root.join("empty")).unwrap();
    std::fs::write(root.join("a.txt"), "x").unwrap();
    let body = format!(
        "assert_eq!(inst.fs_open({EMPTY}, 0b10, 1), 0);
         let e = inst.fs_last();
         let ls = inst.fs_listing(e);
         // a handle, not `100 + the error code`
         assert!((0..100).contains(&ls), \"{{ls}}\");
         // no entries (the end), and asking again is still the end
         assert_eq!(inst.fs_next(ls), -1);
         assert_eq!(inst.fs_next(ls), -1);"
    );
    assert_succeeded(&run(&root, &body));
}

#[test]
fn read_directory_yields_each_entry_once_then_none_and_a_dropped_listing_is_bad_descriptor() {
    let root = fresh_root("edges_exhaust");
    std::fs::write(root.join("a.txt"), "x").unwrap();
    std::fs::write(root.join("b.txt"), "x").unwrap();
    let body = "let ls = inst.fs_listing(0);
         // two entries, each once (5-byte names starting with 'a' 97 and 'b' 98, in any
         // order), then the end for good
         let mut got = [inst.fs_next(ls), inst.fs_next(ls)];
         got.sort();
         assert_eq!(got, [5 | 97 << 8, 5 | 98 << 8]);
         assert_eq!(inst.fs_next(ls), -1);
         assert_eq!(inst.fs_next(ls), -1);
         // a second listing of the same directory starts over, independently
         let again = inst.fs_listing(0);
         assert_ne!(again, ls);
         let first = inst.fs_next(again);
         assert!(first == 5 | 97 << 8 || first == 5 | 98 << 8, \"{first}\");
         // once dropped, the listing is a bad-descriptor 3 and the other is unaffected
         inst.fs_drop_listing(ls);
         assert_eq!(inst.fs_next(ls), 100 + 3);
         let second = inst.fs_next(again);
         assert!(second != first && second > 0, \"{second}\");
         assert_eq!(inst.fs_next(again), -1);";
    assert_succeeded(&run(&root, body));
}

#[test]
fn open_at_through_a_file_is_not_directory_and_a_directory_is_not_a_file_stream() {
    let root = fresh_root("edges_open");
    std::fs::write(root.join("a.txt"), "x").unwrap();
    std::fs::create_dir(root.join("d")).unwrap();
    let body = format!(
        "// a path that goes through a file: not-directory 24
         assert_eq!(inst.fs_open({A_THROUGH}, 0, 1), 100 + 24);
         // the directory flag on a directory is fine
         assert_eq!(inst.fs_open({D}, 0b10, 1), 0);
         // a directory opened without the flag is still a directory: no stream (-1)
         assert_eq!(inst.fs_open({D}, 0, 1), 0);
         let dir = inst.fs_last();
         assert_eq!(inst.fs_stream(dir, 0), -1);"
    );
    assert_succeeded(&run(&root, &body));
}
