//! Writable `wasi:filesystem` on the typed host, with the imports declared in WIT text.
//! Each export of `CORE` returns 0 for success (or a handle) and `100 + the error-code`
//! for an error, so a test reads `100 + 7` as `exist`.

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
  resource output-stream {
    check-write: func() -> result<u64, stream-error>;
    write: func(contents: list<u8>) -> result<_, stream-error>;
    blocking-flush: func() -> result<_, stream-error>;
  }
}
"#;

const FILESYSTEM: &str = r#"
package wasi:filesystem@0.2.0;
interface types {
  use wasi:io/streams@0.2.0.{output-stream, error};
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
  variant new-timestamp { no-change, now, timestamp(datetime) }
  type link-count = u64;
  record descriptor-stat {
    %type: descriptor-type,
    link-count: link-count,
    size: filesize,
    data-access-timestamp: option<datetime>,
    data-modification-timestamp: option<datetime>,
    status-change-timestamp: option<datetime>,
  }
  enum error-code {
    access, would-block, already, bad-descriptor, busy, deadlock, quota, exist, file-too-large,
    illegal-byte-sequence, in-progress, interrupted, invalid, io, is-directory, loop,
    too-many-links, message-size, name-too-long, no-device, no-entry, no-lock,
    insufficient-memory, insufficient-space, not-directory, not-empty, not-recoverable,
    unsupported, no-tty, no-such-device, overflow, not-permitted, pipe, read-only,
    invalid-seek, text-file-busy, cross-device
  }
  resource descriptor {
    get-flags: func() -> result<descriptor-flags, error-code>;
    write-via-stream: func(offset: filesize) -> result<output-stream, error-code>;
    append-via-stream: func() -> result<output-stream, error-code>;
    open-at: func(path-flags: path-flags, path: string, open-flags: open-flags, %flags: descriptor-flags) -> result<descriptor, error-code>;
    set-size: func(size: filesize) -> result<_, error-code>;
    sync: func() -> result<_, error-code>;
    sync-data: func() -> result<_, error-code>;
    rename-at: func(old-path: string, new-descriptor: borrow<descriptor>, new-path: string) -> result<_, error-code>;
    symlink-at: func(old-path: string, new-path: string) -> result<_, error-code>;
    readlink-at: func(path: string) -> result<string, error-code>;
    link-at: func(old-path-flags: path-flags, old-path: string, new-descriptor: borrow<descriptor>, new-path: string) -> result<_, error-code>;
    set-times: func(data-access-timestamp: new-timestamp, data-modification-timestamp: new-timestamp) -> result<_, error-code>;
    set-times-at: func(path-flags: path-flags, path: string, data-access-timestamp: new-timestamp, data-modification-timestamp: new-timestamp) -> result<_, error-code>;
    stat-at: func(path-flags: path-flags, path: string) -> result<descriptor-stat, error-code>;
    create-directory-at: func(path: string) -> result<_, error-code>;
    remove-directory-at: func(path: string) -> result<_, error-code>;
    unlink-file-at: func(path: string) -> result<_, error-code>;
  }
  resource directory-entry-stream {
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
  (import "wasi:io/streams@0.2.0" "[resource-drop]output-stream" (func $drop_out (param i32)))
  (import "wasi:io/streams@0.2.0" "[method]output-stream.check-write" (func $check (param i32 i32)))
  (import "wasi:io/streams@0.2.0" "[method]output-stream.write" (func $write (param i32 i32 i32 i32)))
  (import "wasi:io/streams@0.2.0" "[method]output-stream.blocking-flush" (func $flush (param i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.get-flags" (func $flags (param i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.write-via-stream" (func $wvs (param i32 i64 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.append-via-stream" (func $avs (param i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.open-at" (func $open (param i32 i32 i32 i32 i32 i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.set-size" (func $set_size (param i32 i64 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.sync" (func $sync (param i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.sync-data" (func $sync_data (param i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.rename-at" (func $rename (param i32 i32 i32 i32 i32 i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.symlink-at" (func $symlink (param i32 i32 i32 i32 i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.readlink-at" (func $readlink (param i32 i32 i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.link-at" (func $link (param i32 i32 i32 i32 i32 i32 i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.set-times" (func $set_times (param i32 i32 i64 i32 i32 i64 i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.set-times-at" (func $set_times_at (param i32 i32 i32 i32 i32 i64 i32 i32 i64 i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.stat-at" (func $stat_at (param i32 i32 i32 i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.create-directory-at" (func $mkdir (param i32 i32 i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.remove-directory-at" (func $rmdir (param i32 i32 i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.unlink-file-at" (func $unlink (param i32 i32 i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[resource-drop]descriptor" (func $drop_fd (param i32)))
  (import "wasi:filesystem/types@0.2.0" "[resource-drop]directory-entry-stream" (func (param i32)))
  (import "wasi:filesystem/preopens@0.2.0" "get-directories" (func $dirs (param i32)))
  (memory (export "memory") 1)
  (global $brk (mut i32) (i32.const 2048))
  (global $last (mut i32) (i32.const 0))
  (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32) (local $p i32)
    (local.set $p (i32.and (i32.add (global.get $brk) (i32.sub (local.get 2) (i32.const 1)))
                           (i32.sub (i32.const 0) (local.get 2))))
    (global.set $brk (i32.add (local.get $p) (local.get 3)))
    (local.get $p))
  (data (i32.const 400) "f.txt")
  (data (i32.const 416) "d")
  (data (i32.const 432) "d/x.txt")
  (data (i32.const 448) "g.txt")
  (data (i32.const 464) "ln")
  (data (i32.const 480) "../s.txt")
  (data (i32.const 496) "/abs")
  (data (i32.const 512) "d/e")
  (data (i32.const 544) "hello")
  (data (i32.const 560) "world")
  (data (i32.const 576) "nothere/x")
  (data (i32.const 640) "in_d")
  (data (i32.const 656) "back")

  (func $root (result i32)
    (call $dirs (i32.const 100))
    (i32.load (i32.load (i32.const 100))))
  ;; a `result<_, error-code>` at `at`: 0, or 100 + the error code
  (func $status (param $at i32) (result i32)
    (if (result i32) (i32.load8_u (local.get $at))
      (then (i32.add (i32.const 100) (i32.load8_u (i32.add (local.get $at) (i32.const 1)))))
      (else (i32.const 0))))

  (func (export "fs_stat_at") (param $p i32) (param $n i32) (result i32)
    (call $stat_at (call $root) (i32.const 1) (local.get $p) (local.get $n) (i32.const 600))
    (if (result i32) (i32.load8_u (i32.const 600))
      (then (i32.add (i32.const 100) (i32.load8_u (i32.const 608))))
      (else (i32.load8_u (i32.const 608)))))
  (func (export "fs_size") (result i64) (i64.load (i32.const 624)))
  (func (export "fs_mkdir") (param $p i32) (param $n i32) (result i32)
    (call $mkdir (call $root) (local.get $p) (local.get $n) (i32.const 900))
    (call $status (i32.const 900)))
  (func (export "fs_rmdir") (param $p i32) (param $n i32) (result i32)
    (call $rmdir (call $root) (local.get $p) (local.get $n) (i32.const 900))
    (call $status (i32.const 900)))
  (func (export "fs_unlink") (param $p i32) (param $n i32) (result i32)
    (call $unlink (call $root) (local.get $p) (local.get $n) (i32.const 900))
    (call $status (i32.const 900)))
  ;; open-at from the preopen: 0 (the handle is `fs_last`) or 100 + the error code
  (func (export "fs_open") (param $p i32) (param $n i32) (param $o i32) (param $d i32) (result i32)
    (call $open (call $root) (i32.const 1) (local.get $p) (local.get $n)
                (local.get $o) (local.get $d) (i32.const 200))
    (if (result i32) (i32.load8_u (i32.const 200))
      (then (i32.add (i32.const 100) (i32.load8_u (i32.const 204))))
      (else (global.set $last (i32.load (i32.const 204))) (i32.const 0))))
  (func (export "fs_last") (result i32) (global.get $last))
  (func (export "fs_flags") (param $h i32) (result i32)
    (call $flags (local.get $h) (i32.const 700))
    (if (result i32) (i32.load8_u (i32.const 700))
      (then (i32.add (i32.const 100) (i32.load8_u (i32.const 701))))
      (else (i32.load8_u (i32.const 701)))))
  ;; write-via-stream and append-via-stream: the stream handle, or 1000 + the error code
  (func (export "fs_wstream") (param $h i32) (param $offset i64) (result i32)
    (call $wvs (local.get $h) (local.get $offset) (i32.const 240))
    (if (result i32) (i32.load8_u (i32.const 240))
      (then (i32.add (i32.const 1000) (i32.load8_u (i32.const 244))))
      (else (i32.load (i32.const 244)))))
  (func (export "fs_astream") (param $h i32) (result i32)
    (call $avs (local.get $h) (i32.const 240))
    (if (result i32) (i32.load8_u (i32.const 240))
      (then (i32.add (i32.const 1000) (i32.load8_u (i32.const 244))))
      (else (i32.load (i32.const 244)))))
  ;; the write budget of a stream, or -1
  (func (export "out_check") (param $s i32) (result i64)
    (call $check (local.get $s) (i32.const 256))
    (if (result i64) (i32.load8_u (i32.const 256))
      (then (i64.const -1))
      (else (i64.load (i32.const 264)))))
  ;; write the bytes at (p, n): 0, or 1 for an error
  (func (export "out_write") (param $s i32) (param $p i32) (param $n i32) (result i32)
    (call $write (local.get $s) (local.get $p) (local.get $n) (i32.const 280))
    (i32.load8_u (i32.const 280)))
  (func (export "out_flush") (param $s i32) (result i32)
    (call $flush (local.get $s) (i32.const 280))
    (i32.load8_u (i32.const 280)))
  (func (export "fs_set_size") (param $h i32) (param $size i64) (result i32)
    (call $set_size (local.get $h) (local.get $size) (i32.const 900))
    (call $status (i32.const 900)))
  (func (export "fs_sync") (param $h i32) (result i32)
    (call $sync (local.get $h) (i32.const 900))
    (call $status (i32.const 900)))
  (func (export "fs_sync_data") (param $h i32) (result i32)
    (call $sync_data (local.get $h) (i32.const 900))
    (call $status (i32.const 900)))
  ;; rename-at from the preopen to path (p2, n2) under descriptor `nd`
  (func (export "fs_rename") (param $p i32) (param $n i32) (param $nd i32) (param $p2 i32) (param $n2 i32) (result i32)
    (call $rename (call $root) (local.get $p) (local.get $n) (local.get $nd) (local.get $p2) (local.get $n2) (i32.const 900))
    (call $status (i32.const 900)))
  ;; a symlink at (p2, n2) holding the target (p, n)
  (func (export "fs_symlink") (param $p i32) (param $n i32) (param $p2 i32) (param $n2 i32) (result i32)
    (call $symlink (call $root) (local.get $p) (local.get $n) (local.get $p2) (local.get $n2) (i32.const 900))
    (call $status (i32.const 900)))
  ;; link-at with path flags `f` from the preopen to (p2, n2) under descriptor `nd`
  (func (export "fs_link") (param $f i32) (param $p i32) (param $n i32) (param $nd i32) (param $p2 i32) (param $n2 i32) (result i32)
    (call $link (call $root) (local.get $f) (local.get $p) (local.get $n) (local.get $nd) (local.get $p2) (local.get $n2) (i32.const 900))
    (call $status (i32.const 900)))
  ;; link-at from descriptor `h` (no path flags) to (p2, n2) under descriptor `nd`
  (func (export "fs_link_from") (param $h i32) (param $p i32) (param $n i32) (param $nd i32) (param $p2 i32) (param $n2 i32) (result i32)
    (call $link (local.get $h) (i32.const 0) (local.get $p) (local.get $n) (local.get $nd) (local.get $p2) (local.get $n2) (i32.const 900))
    (call $status (i32.const 900)))
  ;; readlink-at: the target's length << 8 | its first byte, or 100 + the error code
  (func (export "fs_readlink") (param $p i32) (param $n i32) (result i32)
    (call $readlink (call $root) (local.get $p) (local.get $n) (i32.const 740))
    (if (result i32) (i32.load8_u (i32.const 740))
      (then (i32.add (i32.const 100) (i32.load8_u (i32.const 744))))
      (else (i32.or (i32.shl (i32.load (i32.const 748)) (i32.const 8))
                    (i32.load8_u (i32.load (i32.const 744)))))))
  ;; the times are new-timestamps: a tag (no-change 0, now 1, timestamp 2), seconds, nanoseconds
  (func (export "fs_set_times") (param $h i32)
      (param $at i32) (param $as i64) (param $an i32) (param $mt i32) (param $ms i64) (param $mn i32) (result i32)
    (call $set_times (local.get $h) (local.get $at) (local.get $as) (local.get $an)
                     (local.get $mt) (local.get $ms) (local.get $mn) (i32.const 900))
    (call $status (i32.const 900)))
  (func (export "fs_set_times_at") (param $f i32) (param $p i32) (param $n i32)
      (param $at i32) (param $as i64) (param $an i32) (param $mt i32) (param $ms i64) (param $mn i32) (result i32)
    (call $set_times_at (call $root) (local.get $f) (local.get $p) (local.get $n)
                        (local.get $at) (local.get $as) (local.get $an)
                        (local.get $mt) (local.get $ms) (local.get $mn) (i32.const 900))
    (call $status (i32.const 900)))
  (func (export "fs_drop") (param i32) (call $drop_fd (local.get 0)))
  (func (export "out_drop") (param i32) (call $drop_out (local.get 0))))
"#;

// the strings of CORE
const F: &str = "400, 5";
const G: &str = "448, 5";
const LN: &str = "464, 2";
const HELLO_DATA: &str = "544, 5";
const WORLD_DATA: &str = "560, 5";
const DE: &str = "512, 3";
const D: &str = "416, 1";
const DX: &str = "432, 7";
const PARENT_S: &str = "480, 8";
const ABS: &str = "496, 4";
const NOPARENT: &str = "576, 9";
const IN_D: &str = "640, 4";
const BACK: &str = "656, 4";

/// Runs `body` with `inst` on a host whose preopen is `root`, writable or not.
fn run(root: &Path, writable: bool, body: &str) -> std::process::Output {
    let preopen = if writable {
        "with_writable_preopen"
    } else {
        "with_preopen"
    };
    run_program(
        &component_from_wit(&[CLOCKS, IO, FILESYSTEM], WORLD, "w"),
        CORE,
        &format!(
            "TypedHost::new(Vec::new()).{preopen}(std::path::PathBuf::from({:?}))",
            root.to_str().unwrap()
        ),
        body,
    )
}

#[test]
fn create_directory_makes_a_directory_that_stat_sees() {
    let root = fresh_root("mkdir");
    let body = format!(
        "// directory 3; a second create is exist 7, a missing parent no-entry 20
         assert_eq!(inst.fs_mkdir({D}), 0);
         assert_eq!(inst.fs_stat_at({D}), 3);
         assert_eq!(inst.fs_mkdir({D}), 100 + 7);
         assert_eq!(inst.fs_mkdir({NOPARENT}), 100 + 20);
         // `..` and absolute paths are not-permitted 31
         assert_eq!(inst.fs_mkdir({PARENT_S}), 100 + 31);
         assert_eq!(inst.fs_mkdir({ABS}), 100 + 31);"
    );
    assert_succeeded(&run(&root, true, &body));
    assert!(root.join("d").is_dir());
    assert!(!root.parent().unwrap().join("s.txt").exists());
}

#[test]
fn unlink_and_remove_directory_remove_what_they_name() {
    let root = fresh_root("remove");
    std::fs::write(root.join("f.txt"), "x").unwrap();
    std::fs::create_dir(root.join("d")).unwrap();
    std::fs::write(root.join("d/x.txt"), "x").unwrap();
    let body = format!(
        "// unlink of a directory is is-directory 14, remove of a file not-directory 24,
         // of a directory with an entry not-empty 25
         assert_eq!(inst.fs_unlink({D}), 100 + 14);
         assert_eq!(inst.fs_rmdir({F}), 100 + 24);
         assert_eq!(inst.fs_rmdir({D}), 100 + 25);
         assert_eq!(inst.fs_unlink({F}), 0);
         assert_eq!(inst.fs_stat_at({F}), 100 + 20);
         assert_eq!(inst.fs_unlink({F}), 100 + 20);
         assert_eq!(inst.fs_unlink({DX}), 0);
         assert_eq!(inst.fs_rmdir({D}), 0);
         assert_eq!(inst.fs_stat_at({D}), 100 + 20);"
    );
    assert_succeeded(&run(&root, true, &body));
    assert!(!root.join("f.txt").exists() && !root.join("d").exists());
}

#[test]
fn a_read_only_preopen_refuses_directory_changes() {
    let root = fresh_root("ro_dirs");
    std::fs::write(root.join("f.txt"), "x").unwrap();
    let body = format!(
        "// read-only 33
         assert_eq!(inst.fs_mkdir({D}), 100 + 33);
         assert_eq!(inst.fs_unlink({F}), 100 + 33);
         assert_eq!(inst.fs_rmdir({D}), 100 + 33);"
    );
    assert_succeeded(&run(&root, false, &body));
    assert!(root.join("f.txt").exists() && !root.join("d").exists());
}

// open-flags: create 1, directory 2, exclusive 4, truncate 8; descriptor-flags: read 1, write 2

#[test]
fn write_via_stream_writes_a_file_the_host_reads_back() {
    let root = fresh_root("write");
    let body = format!(
        "assert_eq!(inst.fs_open({F}, 1, 2), 0);
         let f = inst.fs_last();
         // a stream handle is clear of the stdio handles 0 to 2
         let s1 = inst.fs_wstream(f, 0);
         assert!((3..1000).contains(&s1), \"{{s1}}\");
         assert_eq!(inst.out_check(s1), 4096);
         assert_eq!(inst.out_write(s1, {HELLO_DATA}), 0);
         assert_eq!(inst.out_flush(s1), 0);
         // a second stream has its own position: \"world\" from offset 3 overwrites the end
         let s2 = inst.fs_wstream(f, 3);
         assert_ne!(s1, s2);
         assert_eq!(inst.out_write(s2, {WORLD_DATA}), 0);
         // the first goes on where it stopped
         assert_eq!(inst.out_write(s1, {WORLD_DATA}), 0);
         // a directory has no stream (is-directory 14), a file not opened for write
         // bad-descriptor 3
         assert_eq!(inst.fs_wstream(0, 0), 1000 + 14);
         assert_eq!(inst.fs_open({F}, 0, 1), 0);
         let ro = inst.fs_last();
         assert_eq!(inst.fs_wstream(ro, 0), 1000 + 3);"
    );
    assert_succeeded(&run(&root, true, &body));
    // s1 wrote "hello" at 0..5, s2 "world" at 3..8, then s1 "world" at 5..10
    assert_eq!(
        std::fs::read_to_string(root.join("f.txt")).unwrap(),
        "helwoworld"
    );
}

#[test]
fn append_via_stream_always_writes_at_the_end() {
    let root = fresh_root("append");
    std::fs::write(root.join("f.txt"), "hello").unwrap();
    let body = format!(
        "assert_eq!(inst.fs_open({F}, 0, 2), 0);
         let f = inst.fs_last();
         let a = inst.fs_astream(f);
         assert!(a >= 3);
         assert_eq!(inst.out_write(a, {WORLD_DATA}), 0);
         // another stream writes elsewhere; the next append still goes to the end
         let w = inst.fs_wstream(f, 0);
         assert_eq!(inst.out_write(w, {WORLD_DATA}), 0);
         assert_eq!(inst.out_write(a, {HELLO_DATA}), 0);
         assert_eq!(inst.fs_astream(0), 1000 + 14);"
    );
    assert_succeeded(&run(&root, true, &body));
    assert_eq!(
        std::fs::read_to_string(root.join("f.txt")).unwrap(),
        "worldworldhello"
    );
}

#[test]
fn open_at_create_exclusive_and_truncate() {
    let root = fresh_root("open_flags");
    std::fs::write(root.join("g.txt"), "hello").unwrap();
    std::fs::create_dir(root.join("d")).unwrap();
    let body = format!(
        "// create makes the file; create + exclusive then finds it: exist 7
         assert_eq!(inst.fs_open({F}, 1 | 4, 2), 0);
         assert_eq!(inst.fs_open({F}, 1 | 4, 2), 100 + 7);
         // create alone keeps what is there, truncate empties it
         assert_eq!(inst.fs_open({G}, 1, 2), 0);
         assert_eq!(inst.fs_stat_at({G}), 6);
         assert_eq!(inst.fs_size(), 5);
         assert_eq!(inst.fs_open({G}, 8, 2), 0);
         assert_eq!(inst.fs_stat_at({G}), 6);
         assert_eq!(inst.fs_size(), 0);
         // a missing file without create is no-entry 20, a directory for write is-directory 14
         assert_eq!(inst.fs_open({NOPARENT}, 0, 2), 100 + 20);
         assert_eq!(inst.fs_open({D}, 0, 2), 100 + 14);
         // a descriptor opened for write reports read and write (3), one for read only 1
         assert_eq!(inst.fs_open({G}, 0, 3), 0);
         let rw = inst.fs_last();
         assert_eq!(inst.fs_flags(rw), 3);
         assert_eq!(inst.fs_open({G}, 0, 1), 0);
         let r = inst.fs_last();
         assert_eq!(inst.fs_flags(r), 1);"
    );
    assert_succeeded(&run(&root, true, &body));
    assert!(root.join("f.txt").is_file());
    assert_eq!(std::fs::read_to_string(root.join("g.txt")).unwrap(), "");
}

#[cfg(unix)]
#[test]
fn writing_through_a_symlink_that_leaves_the_preopen_is_refused() {
    let root = fresh_root("write_escape");
    let secret = root.parent().unwrap().join("secret.txt");
    std::os::unix::fs::symlink(&secret, root.join("ln")).unwrap();
    std::os::unix::fs::symlink("nowhere", root.join("g.txt")).unwrap();
    let body = format!(
        "// not-permitted 31, whether the link leads out or nowhere, and whatever the flags
         assert_eq!(inst.fs_open({LN}, 0, 2), 100 + 31);
         assert_eq!(inst.fs_open({LN}, 1, 2), 100 + 31);
         assert_eq!(inst.fs_open({LN}, 8, 2), 100 + 31);
         assert_eq!(inst.fs_open({G}, 1, 2), 100 + 31);
         assert_eq!(inst.fs_open({PARENT_S}, 1, 2), 100 + 31);
         assert_eq!(inst.fs_open({ABS}, 1, 2), 100 + 31);"
    );
    assert_succeeded(&run(&root, true, &body));
    assert_eq!(std::fs::read_to_string(secret).unwrap(), "no");
    assert!(!root.join("nowhere").exists());
}

#[test]
fn a_dropped_output_stream_frees_its_slot() {
    let root = fresh_root("out_drop");
    let body = format!(
        "assert_eq!(inst.fs_open({F}, 1, 2), 0);
         let f = inst.fs_last();
         let s1 = inst.fs_wstream(f, 0);
         let s2 = inst.fs_astream(f);
         assert!(s1 >= 3 && s2 >= 3 && s1 != s2);
         inst.out_drop(s1);
         // the slot comes back, and the other stream still writes
         assert_eq!(inst.fs_wstream(f, 0), s1);
         assert_eq!(inst.out_write(s2, {HELLO_DATA}), 0);
         // the stdio handles are not slots: dropping them frees nothing
         inst.out_drop(1);
         inst.out_drop(2);
         assert_ne!(inst.fs_wstream(f, 0), s1);"
    );
    assert_succeeded(&run(&root, true, &body));
    assert_eq!(
        std::fs::read_to_string(root.join("f.txt")).unwrap(),
        "hello"
    );
}

#[test]
fn set_size_truncates_and_zero_extends_and_sync_succeeds() {
    let root = fresh_root("set_size");
    std::fs::write(root.join("f.txt"), "hello").unwrap();
    let body = format!(
        "assert_eq!(inst.fs_open({F}, 0, 2), 0);
         let f = inst.fs_last();
         assert_eq!(inst.fs_set_size(f, 2), 0);
         assert_eq!(inst.fs_stat_at({F}), 6);
         assert_eq!(inst.fs_size(), 2);
         assert_eq!(inst.fs_set_size(f, 4), 0);
         assert_eq!(inst.fs_stat_at({F}), 6);
         assert_eq!(inst.fs_size(), 4);
         // a descriptor not opened for write is bad-descriptor 3, a directory is-directory 14
         assert_eq!(inst.fs_open({F}, 0, 1), 0);
         let ro = inst.fs_last();
         assert_eq!(inst.fs_set_size(ro, 0), 100 + 3);
         assert_eq!(inst.fs_set_size(0, 0), 100 + 14);
         // sync and sync-data work on a file and on a directory, bad-descriptor 3 otherwise
         assert_eq!(inst.fs_sync(f), 0);
         assert_eq!(inst.fs_sync_data(f), 0);
         assert_eq!(inst.fs_sync(0), 0);
         assert_eq!(inst.fs_sync_data(0), 0);
         assert_eq!(inst.fs_sync(77), 100 + 3);
         assert_eq!(inst.fs_sync_data(77), 100 + 3);"
    );
    assert_succeeded(&run(&root, true, &body));
    assert_eq!(std::fs::read(root.join("f.txt")).unwrap(), b"he\0\0");
}

#[test]
fn rename_moves_an_entry_even_into_another_directory() {
    let root = fresh_root("rename");
    std::fs::write(root.join("f.txt"), "one").unwrap();
    std::fs::write(root.join("g.txt"), "two").unwrap();
    std::fs::create_dir(root.join("d")).unwrap();
    let body = format!(
        "// an existing target file is replaced (it keeps the content one); a missing source is no-entry 20
         assert_eq!(inst.fs_rename({F}, 0, {G}), 0);
         assert_eq!(inst.fs_stat_at({F}), 100 + 20);
         assert_eq!(inst.fs_rename({F}, 0, {G}), 100 + 20);
         // into the directory `d`, through its descriptor
         assert_eq!(inst.fs_open({D}, 2, 1), 0);
         let d = inst.fs_last();
         assert_eq!(inst.fs_rename({G}, d, {F}), 0);
         assert_eq!(inst.fs_stat_at({DX}), 100 + 20);
         // `..` and absolute paths are not-permitted 31, for either name
         assert_eq!(inst.fs_rename({F}, 0, {PARENT_S}), 100 + 31);
         assert_eq!(inst.fs_rename({ABS}, 0, {F}), 100 + 31);
         assert_eq!(inst.fs_rename({PARENT_S}, 0, {F}), 100 + 31);"
    );
    assert_succeeded(&run(&root, true, &body));
    assert!(!root.join("g.txt").exists() && !root.join("f.txt").exists());
    assert_eq!(
        std::fs::read_to_string(root.join("d/f.txt")).unwrap(),
        "one"
    );
    assert!(!root.parent().unwrap().join("s.txt").exists());
}

#[cfg(unix)]
#[test]
fn symlink_at_makes_relative_links_only_and_a_symlink_cannot_be_renamed() {
    let root = fresh_root("symlink");
    std::fs::write(root.join("f.txt"), "one").unwrap();
    let body = format!(
        "assert_eq!(inst.fs_symlink({F}, {LN}), 0);
         // the name is taken: exist 7; a target that leaves the preopen or is absolute is
         // not-permitted 31; an empty target no-entry 20; the name is checked as well
         assert_eq!(inst.fs_symlink({F}, {LN}), 100 + 7);
         assert_eq!(inst.fs_symlink({PARENT_S}, {G}), 100 + 31);
         assert_eq!(inst.fs_symlink({ABS}, {G}), 100 + 31);
         assert_eq!(inst.fs_symlink(400, 0, {G}), 100 + 20);
         assert_eq!(inst.fs_symlink({F}, {PARENT_S}), 100 + 31);
         // the link is followed when it is opened, and writing through it changes the file
         assert_eq!(inst.fs_open({LN}, 0, 2), 0);
         let w = inst.fs_astream(inst.fs_last());
         assert_eq!(inst.out_write(w, {HELLO_DATA}), 0);
         // a symlink is not renamed (not-permitted 31), but it can be removed
         assert_eq!(inst.fs_rename({LN}, 0, {G}), 100 + 31);
         assert_eq!(inst.fs_unlink({LN}), 0);"
    )
    .replace(
        "inst.fs_astream(inst.fs_last())",
        "{ let l = inst.fs_last(); inst.fs_astream(l) }",
    );
    assert_succeeded(&run(&root, true, &body));
    assert_eq!(
        std::fs::read_to_string(root.join("f.txt")).unwrap(),
        "onehello"
    );
    assert!(root.join("ln").symlink_metadata().is_err());
    assert!(!root.join("g.txt").exists());
}

#[test]
fn link_at_makes_a_hard_link() {
    let root = fresh_root("link");
    std::fs::write(root.join("f.txt"), "one").unwrap();
    std::fs::create_dir(root.join("d")).unwrap();
    let body = format!(
        "assert_eq!(inst.fs_link(0, {F}, 0, {G}), 0);
         // the new name is taken: exist 7; a directory cannot be linked: not-permitted 31
         assert_eq!(inst.fs_link(0, {F}, 0, {G}), 100 + 7);
         assert_eq!(inst.fs_link(0, {D}, 0, {DE}), 100 + 31);
         assert_eq!(inst.fs_link(0, {PARENT_S}, 0, {DE}), 100 + 31);
         assert_eq!(inst.fs_link(0, {F}, 0, {PARENT_S}), 100 + 31);
         // into a directory, through its descriptor
         assert_eq!(inst.fs_open({D}, 2, 1), 0);
         let d = inst.fs_last();
         assert_eq!(inst.fs_link(0, {F}, d, {F}), 0);"
    );
    assert_succeeded(&run(&root, true, &body));
    assert_eq!(std::fs::read_to_string(root.join("g.txt")).unwrap(), "one");
    assert_eq!(
        std::fs::read_to_string(root.join("d/f.txt")).unwrap(),
        "one"
    );
    // writing through one name shows through the others
    std::fs::write(root.join("g.txt"), "two").unwrap();
    assert_eq!(std::fs::read_to_string(root.join("f.txt")).unwrap(), "two");
}

#[cfg(unix)]
#[test]
fn link_at_follows_a_symlink_only_when_asked() {
    let root = fresh_root("link_symlink");
    std::fs::write(root.join("f.txt"), "one").unwrap();
    std::os::unix::fs::symlink("f.txt", root.join("ln")).unwrap();
    let body = format!(
        "// without symlink-follow a symlink is unsupported 27, with it the target is linked
         assert_eq!(inst.fs_link(0, {LN}, 0, {G}), 100 + 27);
         assert_eq!(inst.fs_link(1, {LN}, 0, {G}), 0);"
    );
    assert_succeeded(&run(&root, true, &body));
    assert!(root.join("g.txt").symlink_metadata().unwrap().is_file());
    assert_eq!(std::fs::read_to_string(root.join("g.txt")).unwrap(), "one");
}

#[cfg(unix)]
#[test]
fn readlink_at_shows_the_relative_target_of_a_symlink_the_guest_made() {
    let root = fresh_root("readlink");
    std::fs::write(root.join("f.txt"), "one").unwrap();
    let body = format!(
        "assert_eq!(inst.fs_symlink({F}, {LN}), 0);
         // `f.txt`: 5 bytes, 'f' 102; a file is no symlink (invalid 12), nothing there is no-entry 20
         assert_eq!(inst.fs_readlink({LN}), 5 << 8 | 102);
         assert_eq!(inst.fs_readlink({F}), 100 + 12);
         assert_eq!(inst.fs_readlink({G}), 100 + 20);
         // a path that leaves the preopen is not permitted
         assert_eq!(inst.fs_readlink({PARENT_S}), 100 + 31);"
    );
    assert_succeeded(&run(&root, true, &body));
    assert_eq!(
        std::fs::read_link(root.join("ln")).unwrap(),
        Path::new("f.txt")
    );
}

#[cfg(unix)]
#[test]
fn link_at_between_two_directory_descriptors_links_inside_the_new_one() {
    use std::os::unix::fs::MetadataExt;
    let root = fresh_root("link_across");
    std::fs::write(root.join("f.txt"), "one").unwrap();
    std::fs::create_dir(root.join("d")).unwrap();
    let body = format!(
        "assert_eq!(inst.fs_open({D}, 2, 1), 0);
         let d = inst.fs_last();
         // root/f.txt -> d/in_d, then d/in_d -> root/back (the old path is under `d`)
         assert_eq!(inst.fs_link(0, {F}, d, {IN_D}), 0);
         assert_eq!(inst.fs_link_from(d, {IN_D}, 0, {BACK}), 0);
         // a new name that leaves the new directory is refused: not-permitted 31
         assert_eq!(inst.fs_link(0, {F}, d, {PARENT_S}), 100 + 31);
         assert_eq!(inst.fs_link(0, {F}, d, {ABS}), 100 + 31);"
    );
    assert_succeeded(&run(&root, true, &body));
    let ino = |name: &str| std::fs::metadata(root.join(name)).unwrap().ino();
    assert_eq!(ino("d/in_d"), ino("f.txt"));
    assert_eq!(ino("back"), ino("f.txt"));
    assert!(!root.join("in_d").exists());
    assert!(!root.join("s.txt").exists());
}

/// The access and modification times of `path` in whole seconds since the epoch.
fn times(path: &Path) -> (u64, u64) {
    let meta = std::fs::metadata(path).unwrap();
    let secs =
        |t: std::time::SystemTime| t.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    (
        secs(meta.accessed().unwrap()),
        secs(meta.modified().unwrap()),
    )
}

#[test]
fn set_times_sets_the_times_of_a_descriptor() {
    let root = fresh_root("set_times");
    std::fs::write(root.join("f.txt"), "one").unwrap();
    let body = format!(
        "assert_eq!(inst.fs_open({F}, 0, 2), 0);
         let f = inst.fs_last();
         // timestamp 2 for both
         assert_eq!(inst.fs_set_times(f, 2, 1_000_000_000, 5, 2, 1_500_000_000, 7), 0);
         // no-change 0 keeps the access time, now 1 moves the modification time to now
         assert_eq!(inst.fs_set_times(f, 0, 0, 0, 1, 0, 0), 0);
         // a directory (the preopen) takes times too
         assert_eq!(inst.fs_set_times(0, 2, 1_100_000_000, 0, 2, 1_200_000_000, 0), 0);
         // a file not opened for write is bad-descriptor 3; nanoseconds of a second or more
         // and a time too far ahead are invalid 12
         assert_eq!(inst.fs_open({F}, 0, 1), 0);
         let ro = inst.fs_last();
         assert_eq!(inst.fs_set_times(ro, 0, 0, 0, 1, 0, 0), 100 + 3);
         assert_eq!(inst.fs_set_times(f, 2, 1, 1_000_000_000, 0, 0, 0), 100 + 12);
         assert_eq!(inst.fs_set_times(f, 0, 0, 0, 2, -1, 0), 100 + 12);"
    );
    assert_succeeded(&run(&root, true, &body));
    let (access, modified) = times(&root.join("f.txt"));
    assert_eq!(access, 1_000_000_000);
    assert!(modified > 1_600_000_000, "{modified}");
    assert_eq!(times(&root), (1_100_000_000, 1_200_000_000));
}

#[test]
fn set_times_at_sets_the_times_of_a_path() {
    let root = fresh_root("set_times_at");
    std::fs::write(root.join("f.txt"), "one").unwrap();
    let body = format!(
        "// symlink-follow is bit 0 of the path flags
         assert_eq!(inst.fs_set_times_at(1, {F}, 2, 1_000_000_000, 0, 2, 1_500_000_000, 0), 0);
         assert_eq!(inst.fs_set_times_at(0, {F}, 2, 1_100_000_000, 0, 2, 1_600_000_000, 0), 0);
         assert_eq!(inst.fs_set_times_at(0, {G}, 0, 0, 0, 1, 0, 0), 100 + 20);
         assert_eq!(inst.fs_set_times_at(0, {PARENT_S}, 0, 0, 0, 1, 0, 0), 100 + 31);
         assert_eq!(inst.fs_set_times_at(1, {ABS}, 0, 0, 0, 1, 0, 0), 100 + 31);
         assert_eq!(inst.fs_set_times_at(0, {F}, 2, 0, 1_000_000_000, 0, 0, 0), 100 + 12);"
    );
    assert_succeeded(&run(&root, true, &body));
    assert_eq!(times(&root.join("f.txt")), (1_100_000_000, 1_600_000_000));
}

#[cfg(unix)]
#[test]
fn set_times_at_follows_a_symlink_only_when_asked() {
    let root = fresh_root("set_times_link");
    std::fs::write(root.join("f.txt"), "one").unwrap();
    std::os::unix::fs::symlink("f.txt", root.join("ln")).unwrap();
    let body = format!(
        "// std cannot set the times of a symlink itself: unsupported 27
         assert_eq!(inst.fs_set_times_at(0, {LN}, 2, 1_000_000_000, 0, 2, 1_500_000_000, 0), 100 + 27);
         assert_eq!(inst.fs_set_times_at(1, {LN}, 2, 1_000_000_000, 0, 2, 1_500_000_000, 0), 0);"
    );
    assert_succeeded(&run(&root, true, &body));
    assert_eq!(times(&root.join("f.txt")), (1_000_000_000, 1_500_000_000));
}

#[test]
fn a_read_only_preopen_answers_read_only_to_every_changing_method() {
    let root = fresh_root("read_only");
    std::fs::write(root.join("f.txt"), "one").unwrap();
    std::fs::create_dir(root.join("d")).unwrap();
    let body = format!(
        "// read-only 33
         assert_eq!(inst.fs_mkdir({DE}), 100 + 33);
         assert_eq!(inst.fs_rmdir({D}), 100 + 33);
         assert_eq!(inst.fs_unlink({F}), 100 + 33);
         assert_eq!(inst.fs_rename({F}, 0, {G}), 100 + 33);
         assert_eq!(inst.fs_symlink({F}, {LN}), 100 + 33);
         assert_eq!(inst.fs_link(0, {F}, 0, {G}), 100 + 33);
         assert_eq!(inst.fs_set_times_at(1, {F}, 0, 0, 0, 1, 0, 0), 100 + 33);
         // open-at with any change or the write flag
         assert_eq!(inst.fs_open({F}, 0, 2), 100 + 33);
         assert_eq!(inst.fs_open({G}, 1, 1), 100 + 33);
         assert_eq!(inst.fs_open({F}, 4, 1), 100 + 33);
         assert_eq!(inst.fs_open({F}, 8, 1), 100 + 33);
         // a descriptor that reads: its writing methods are refused as well
         assert_eq!(inst.fs_open({F}, 0, 1), 0);
         let f = inst.fs_last();
         assert_eq!(inst.fs_wstream(f, 0), 1000 + 33);
         assert_eq!(inst.fs_astream(f), 1000 + 33);
         assert_eq!(inst.fs_set_size(f, 0), 100 + 33);
         assert_eq!(inst.fs_set_times(f, 0, 0, 0, 1, 0, 0), 100 + 33);
         // sync changes nothing
         assert_eq!(inst.fs_sync(f), 0);
         assert_eq!(inst.fs_sync_data(0), 0);"
    );
    assert_succeeded(&run(&root, false, &body));
    assert_eq!(std::fs::read_to_string(root.join("f.txt")).unwrap(), "one");
    let mut names: Vec<_> = std::fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, ["d", "f.txt"]);
}
