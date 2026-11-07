//! Read-only `wasi:filesystem` on the typed host, with the imports declared in WIT text.

use std::path::Path;

use crate::wasip2_typed_host::{assert_succeeded, component_from_wit, run_program};

const CLOCKS: &str = r#"
package wasi:clocks@0.2.0;
interface wall-clock {
  record datetime { seconds: u64, nanoseconds: u32 }
}
"#;

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
  type link-count = u64;
  record descriptor-stat {
    %type: descriptor-type,
    link-count: link-count,
    size: filesize,
    data-access-timestamp: option<datetime>,
    data-modification-timestamp: option<datetime>,
    status-change-timestamp: option<datetime>,
  }
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
    get-flags: func() -> result<descriptor-flags, error-code>;
    read-directory: func() -> result<directory-entry-stream, error-code>;
    stat: func() -> result<descriptor-stat, error-code>;
    stat-at: func(path-flags: path-flags, path: string) -> result<descriptor-stat, error-code>;
    open-at: func(path-flags: path-flags, path: string, open-flags: open-flags, %flags: descriptor-flags) -> result<descriptor, error-code>;
    readlink-at: func(path: string) -> result<string, error-code>;
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

const WORLD: &str = r#"
package test:t;
world w {
  import wasi:filesystem/preopens@0.2.0;
  import wasi:filesystem/types@0.2.0;
  import wasi:io/streams@0.2.0;
}
"#;

const CORE: &str = r#"
(module
  (import "wasi:io/error@0.2.0" "[resource-drop]error" (func (param i32)))
  (import "wasi:io/poll@0.2.0" "[resource-drop]pollable" (func (param i32)))
  (import "wasi:io/streams@0.2.0" "[method]input-stream.read" (func $read (param i32 i64 i32)))
  (import "wasi:io/streams@0.2.0" "[resource-drop]input-stream" (func $drop_stream (param i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.read-via-stream" (func $rvs (param i32 i64 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.get-flags" (func $flags (param i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.read-directory" (func $readdir (param i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.stat" (func $stat (param i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.stat-at" (func $stat_at (param i32 i32 i32 i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.open-at" (func $open (param i32 i32 i32 i32 i32 i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.readlink-at" (func $readlink (param i32 i32 i32 i32)))
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
  (data (i32.const 300) "hello.txt")
  (data (i32.const 320) "sub")
  (data (i32.const 330) "missing")
  (data (i32.const 340) "../x")
  (data (i32.const 350) "/etc/passwd")
  (data (i32.const 370) "link.txt")
  (data (i32.const 380) "ln.txt")
  (data (i32.const 390) "big.txt")

  (func $root (result i32)
    (call $dirs (i32.const 100))
    (i32.load (i32.load (i32.const 100))))
  ;; 100 + the error-code byte at `at`
  (func $code (param $at i32) (result i32)
    (i32.add (i32.const 100) (i32.load8_u (local.get $at))))

  ;; 0 without a preopen; with one: count << 16 | name length << 8 | the name's first byte
  (func (export "fs_preopen") (result i32)
    (call $dirs (i32.const 100))
    (if (i32.eqz (i32.load (i32.const 104))) (then (return (i32.const 0))))
    (i32.or (i32.or (i32.shl (i32.load (i32.const 104)) (i32.const 16))
                    (i32.shl (i32.load offset=8 (i32.load (i32.const 100))) (i32.const 8)))
            (i32.load8_u (i32.load offset=4 (i32.load (i32.const 100))))))

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
  ;; read: -1 at the end of the stream, else the length | the first four bytes << 32
  (func (export "fs_read") (param $s i32) (param $len i64) (result i64)
    (call $read (local.get $s) (local.get $len) (i32.const 260))
    (if (result i64) (i32.load8_u (i32.const 260))
      (then (i64.const -1))
      (else
        (i64.or
          (i64.extend_i32_u (i32.load (i32.const 268)))
          (if (result i64) (i32.load (i32.const 268))
            (then (i64.shl (i64.extend_i32_u (i32.load (i32.load (i32.const 264)))) (i64.const 32)))
            (else (i64.const 0)))))))

  ;; stat of the preopen's path / of a descriptor: the type byte, or 100 + the error code
  (func (export "fs_stat_at") (param $p i32) (param $n i32) (result i32)
    (call $stat_at (call $root) (i32.const 1) (local.get $p) (local.get $n) (i32.const 600))
    (if (result i32) (i32.load8_u (i32.const 600))
      (then (call $code (i32.const 608)))
      (else (i32.load8_u (i32.const 608)))))
  (func (export "fs_stat") (param $h i32) (result i32)
    (call $stat (local.get $h) (i32.const 600))
    (if (result i32) (i32.load8_u (i32.const 600))
      (then (call $code (i32.const 608)))
      (else (i32.load8_u (i32.const 608)))))
  ;; the fields of the last stat
  (func (export "fs_nlink") (result i64) (i64.load (i32.const 616)))
  (func (export "fs_size") (result i64) (i64.load (i32.const 624)))
  (func (export "fs_mtime") (result i64)
    (if (result i64) (i32.load8_u (i32.const 656)) (then (i64.load (i32.const 664))) (else (i64.const 0))))
  (func (export "fs_ctime_present") (result i32) (i32.load8_u (i32.const 680)))

  (func (export "fs_flags") (param $h i32) (result i32)
    (call $flags (local.get $h) (i32.const 700))
    (if (result i32) (i32.load8_u (i32.const 700))
      (then (call $code (i32.const 701)))
      (else (i32.load8_u (i32.const 701)))))

  (func (export "fs_hash") (param $h i32) (result i64)
    (call $hash (local.get $h) (i32.const 720))
    (if (result i64) (i32.load8_u (i32.const 720)) (then (i64.const 0)) (else (i64.load (i32.const 728)))))
  (func (export "fs_hash_at") (param $p i32) (param $n i32) (result i64)
    (call $hash_at (call $root) (i32.const 1) (local.get $p) (local.get $n) (i32.const 720))
    (if (result i64) (i32.load8_u (i32.const 720)) (then (i64.const 0)) (else (i64.load (i32.const 728)))))

  ;; readlink-at: the target's length << 8 | its first byte, or 100 + the error code
  (func (export "fs_readlink") (param $p i32) (param $n i32) (result i32)
    (call $readlink (call $root) (local.get $p) (local.get $n) (i32.const 740))
    (if (result i32) (i32.load8_u (i32.const 740))
      (then (call $code (i32.const 744)))
      (else (i32.or (i32.shl (i32.load (i32.const 748)) (i32.const 8))
                    (i32.load8_u (i32.load (i32.const 744)))))))

  ;; read-directory: the listing's handle, or -1
  (func (export "fs_listing") (param $h i32) (result i32)
    (call $readdir (local.get $h) (i32.const 800))
    (if (result i32) (i32.load8_u (i32.const 800))
      (then (i32.const -1))
      (else (i32.load (i32.const 804)))))
  ;; the whole listing of a directory: the number of entries | the sum of their first
  ;; bytes << 8 (each a regular file with a 5-byte name), or 100 + the error code
  (func (export "fs_readdir") (param $h i32) (result i32) (local $s i32) (local $count i32) (local $sum i32)
    (call $readdir (local.get $h) (i32.const 800))
    (if (i32.load8_u (i32.const 800)) (then (return (call $code (i32.const 804)))))
    (local.set $s (i32.load (i32.const 804)))
    (loop $next
      (call $entry (local.get $s) (i32.const 820))
      (if (i32.load8_u (i32.const 820)) (then (return (i32.const -2))))
      (if (i32.load8_u (i32.const 824))
        (then
          (if (i32.ne (i32.load8_u (i32.const 828)) (i32.const 6)) (then (return (i32.const -3))))
          (if (i32.ne (i32.load (i32.const 836)) (i32.const 5)) (then (return (i32.const -4))))
          (local.set $sum (i32.add (local.get $sum) (i32.load8_u (i32.load (i32.const 832)))))
          (local.set $count (i32.add (local.get $count) (i32.const 1)))
          (br $next))))
    (call $drop_ls (local.get $s))
    (i32.or (local.get $count) (i32.shl (local.get $sum) (i32.const 8))))

  (func (export "fs_drop") (param i32) (call $drop_fd (local.get 0)))
  (func (export "fs_drop_stream") (param i32) (call $drop_stream (local.get 0)))
  (func (export "fs_drop_listing") (param i32) (call $drop_ls (local.get 0))))
"#;

// the strings of CORE
const HELLO: &str = "300, 9";
const SUB: &str = "320, 3";
const MISSING: &str = "330, 7";
const PARENT: &str = "340, 4";
const ABSOLUTE: &str = "350, 11";
const OUTSIDE_LINK: &str = "370, 8";
const INSIDE_LINK: &str = "380, 6";
const BIG: &str = "390, 7";

/// Runs `body` with `inst` on a host whose preopen is `root` (`None`: no preopen).
fn run(root: Option<&Path>, body: &str) -> std::process::Output {
    let preopen = root.map_or(String::new(), |root| {
        format!(
            ".with_preopen(std::path::PathBuf::from({:?}))",
            root.to_str().unwrap()
        )
    });
    run_program(
        &component_from_wit(&[CLOCKS, IO, FILESYSTEM], WORLD, "w"),
        CORE,
        &format!("TypedHost::new(Vec::new()){preopen}"),
        body,
    )
}

/// An empty directory of its own, with the path of a file next to it.
fn fresh_root(name: &str) -> std::path::PathBuf {
    let parent =
        std::env::temp_dir().join(format!("wasm2rs_typed_fs_{}_{name}", std::process::id()));
    std::fs::remove_dir_all(&parent).ok();
    let root = parent.join("root");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(parent.join("secret.txt"), "no").unwrap();
    root
}

#[test]
fn the_preopen_is_one_directory_named_slash() {
    let root = fresh_root("preopen");
    assert_succeeded(&run(
        Some(&root),
        "assert_eq!(inst.fs_preopen(), 1 << 16 | 1 << 8 | 47);",
    ));
    assert_succeeded(&run(None, "assert_eq!(inst.fs_preopen(), 0);"));
}

#[test]
fn a_file_is_read_through_open_at_read_via_stream_and_read() {
    let root = fresh_root("read");
    std::fs::write(root.join("hello.txt"), "hello fs").unwrap();
    std::fs::write(root.join("big.txt"), vec![b'x'; 5000]).unwrap();
    let body = format!(
        "assert_eq!(inst.fs_open({HELLO}, 0, 1), 0);
         let f = inst.fs_last();
         let s = inst.fs_stream(f, 0);
         // clear of the stdio handles 0 to 2
         assert!(s >= 3, \"{{s}}\");
         // \"hell\", then the rest, then the end of the file
         assert_eq!(inst.fs_read(s, 4), 4 | 0x6c6c6568 << 32);
         assert_eq!(inst.fs_read(s, -1), 4 | 0x7366206f << 32);
         assert_eq!(inst.fs_read(s, 1), -1);
         // a second stream at offset 6 sees \"fs\"; an empty read is an empty list
         let t = inst.fs_stream(f, 6);
         assert_ne!(t, s);
         assert_eq!(inst.fs_read(t, 0), 0);
         assert_eq!(inst.fs_read(t, 8), 2 | 0x7366 << 32);
         // a directory has no stream
         assert_eq!(inst.fs_stream(0, 0), -1);
         // a read returns at most 4096 bytes
         assert_eq!(inst.fs_open({BIG}, 0, 1), 0);
         let big = inst.fs_last();
         let b = inst.fs_stream(big, 0);
         assert_eq!(inst.fs_read(b, -1) & 0xffff_ffff, 4096);
         assert_eq!(inst.fs_read(b, -1) & 0xffff_ffff, 904);"
    );
    assert_succeeded(&run(Some(&root), &body));
}

#[test]
fn stat_gives_the_type_size_and_times_of_files_and_directories() {
    let root = fresh_root("stat");
    std::fs::write(root.join("hello.txt"), "hello fs").unwrap();
    std::fs::create_dir(root.join("sub")).unwrap();
    let body = format!(
        "// regular file 6, directory 3
         assert_eq!(inst.fs_stat_at({HELLO}), 6);
         assert_eq!(inst.fs_size(), 8);
         assert_eq!(inst.fs_nlink(), 1);
         assert!(inst.fs_mtime() > 1_700_000_000);
         assert_eq!(inst.fs_ctime_present(), 0);
         assert_eq!(inst.fs_stat_at({SUB}), 3);
         assert_eq!(inst.fs_stat(0), 3);
         // a descriptor of the file stats the file
         assert_eq!(inst.fs_open({HELLO}, 0, 1), 0);
         let f = inst.fs_last();
         assert_eq!(inst.fs_stat(f), 6);
         assert_eq!(inst.fs_size(), 8);
         // every descriptor is `read`
         assert_eq!(inst.fs_flags(0), 1);
         assert_eq!(inst.fs_flags(f), 1);
         // the same file hashes alike, another file differently
         let h = inst.fs_hash_at({HELLO});
         assert_ne!(h, 0);
         assert_eq!(inst.fs_hash(f), h);
         assert_ne!(inst.fs_hash_at({SUB}), h);
         // a descriptor that was never opened
         assert_eq!(inst.fs_stat(77), 103);
         assert_eq!(inst.fs_flags(77), 103);"
    );
    assert_succeeded(&run(Some(&root), &body));
}

#[test]
fn read_directory_lists_a_two_entry_directory() {
    let root = fresh_root("listing");
    std::fs::create_dir(root.join("sub")).unwrap();
    std::fs::write(root.join("sub/b.txt"), "b").unwrap();
    std::fs::write(root.join("sub/c.txt"), "c").unwrap();
    std::fs::write(root.join("hello.txt"), "hello fs").unwrap();
    let body = format!(
        "assert_eq!(inst.fs_open({SUB}, 0b10, 1), 0);
         let d = inst.fs_last();
         // two regular files, `b.txt` (98) and `c.txt` (99)
         assert_eq!(inst.fs_readdir(d), 2 | (98 + 99) << 8);
         // the preopen holds `hello.txt` and `sub`; the listing of a file is not-directory
         assert_eq!(inst.fs_open({HELLO}, 0, 1), 0);
         let f = inst.fs_last();
         assert_eq!(inst.fs_readdir(f), 100 + 24);
         // opening a file as a directory is not-directory too
         assert_eq!(inst.fs_open({HELLO}, 0b10, 1), 100 + 24);"
    );
    assert_succeeded(&run(Some(&root), &body));
}

#[test]
fn missing_files_are_no_entry_and_writes_are_read_only() {
    let root = fresh_root("refuse");
    std::fs::write(root.join("hello.txt"), "hello fs").unwrap();
    let body = format!(
        "// no-entry 20
         assert_eq!(inst.fs_open({MISSING}, 0, 1), 100 + 20);
         assert_eq!(inst.fs_stat_at({MISSING}), 100 + 20);
         // read-only 33: write, create, truncate and mutate-directory flags, even for a
         // file that does not exist
         assert_eq!(inst.fs_open({HELLO}, 0, 0b10), 100 + 33);
         assert_eq!(inst.fs_open({HELLO}, 0, 0b10_0000), 100 + 33);
         assert_eq!(inst.fs_open({HELLO}, 0b1, 1), 100 + 33);
         assert_eq!(inst.fs_open({HELLO}, 0b100, 1), 100 + 33);
         assert_eq!(inst.fs_open({HELLO}, 0b1000, 1), 100 + 33);
         assert_eq!(inst.fs_open({MISSING}, 0b1, 0b11), 100 + 33);
         // reading is fine, and refusing wrote nothing
         assert_eq!(inst.fs_open({HELLO}, 0, 1), 0);"
    );
    assert_succeeded(&run(Some(&root), &body));
    assert_eq!(
        std::fs::read_to_string(root.join("hello.txt")).unwrap(),
        "hello fs"
    );
    assert!(!root.join("missing").exists());
}

#[cfg(unix)]
#[test]
fn paths_leaving_the_preopen_are_not_permitted() {
    let root = fresh_root("escape");
    std::fs::write(root.join("hello.txt"), "hello fs").unwrap();
    std::os::unix::fs::symlink(
        root.parent().unwrap().join("secret.txt"),
        root.join("link.txt"),
    )
    .unwrap();
    std::os::unix::fs::symlink("hello.txt", root.join("ln.txt")).unwrap();
    let body = format!(
        "// not-permitted 31: `..`, an absolute path, a symlink out of the preopen
         assert_eq!(inst.fs_open({PARENT}, 0, 1), 100 + 31);
         assert_eq!(inst.fs_open({ABSOLUTE}, 0, 1), 100 + 31);
         assert_eq!(inst.fs_open({OUTSIDE_LINK}, 0, 1), 100 + 31);
         assert_eq!(inst.fs_stat_at({PARENT}), 100 + 31);
         assert_eq!(inst.fs_stat_at({ABSOLUTE}), 100 + 31);
         assert_eq!(inst.fs_stat_at({OUTSIDE_LINK}), 100 + 31);
         assert_eq!(inst.fs_hash_at({OUTSIDE_LINK}), 0);
         assert_eq!(inst.fs_readlink({PARENT}), 100 + 31);
         // a symlink to a file inside is followed
         assert_eq!(inst.fs_stat_at({INSIDE_LINK}), 6);
         assert_eq!(inst.fs_open({INSIDE_LINK}, 0, 1), 0);
         // readlink-at shows a relative target (`hello.txt`: 9 bytes, 'h' 104), not a file
         // (invalid 12), and refuses an absolute target
         assert_eq!(inst.fs_readlink({INSIDE_LINK}), 9 << 8 | 104);
         assert_eq!(inst.fs_readlink({HELLO}), 100 + 12);
         assert_eq!(inst.fs_readlink({OUTSIDE_LINK}), 100 + 31);"
    );
    assert_succeeded(&run(Some(&root), &body));
}

#[test]
fn dropped_handles_are_reused_and_the_preopen_is_never_freed() {
    let root = fresh_root("drop");
    std::fs::write(root.join("hello.txt"), "hello fs").unwrap();
    std::fs::create_dir(root.join("sub")).unwrap();
    let body = format!(
        "// a descriptor: the slot comes back once dropped, but not while it is open
         assert_eq!(inst.fs_open({HELLO}, 0, 1), 0);
         let f1 = inst.fs_last();
         assert_eq!(inst.fs_open({HELLO}, 0, 1), 0);
         let f2 = inst.fs_last();
         assert!(f1 != 0 && f2 != 0 && f1 != f2);
         inst.fs_drop(f1);
         assert_eq!(inst.fs_open({HELLO}, 0, 1), 0);
         assert_eq!(inst.fs_last(), f1);
         assert_eq!(inst.fs_stat(f2), 6);
         // an input stream: reused once dropped, never a stdio handle, and the stream still open
         // keeps its own position
         let s1 = inst.fs_stream(f1, 0);
         let s2 = inst.fs_stream(f1, 0);
         assert!(s1 >= 3 && s2 >= 3 && s1 != s2);
         assert_eq!(inst.fs_read(s2, 4), 4 | 0x6c6c6568 << 32);
         inst.fs_drop_stream(s1);
         assert_eq!(inst.fs_stream(f1, 6), s1);
         assert_eq!(inst.fs_read(s1, 8), 2 | 0x7366 << 32);
         assert_eq!(inst.fs_read(s2, 4), 4 | 0x7366206f << 32);
         // a directory listing
         let l1 = inst.fs_listing(0);
         let l2 = inst.fs_listing(0);
         assert_ne!(l1, l2);
         inst.fs_drop_listing(l1);
         assert_eq!(inst.fs_listing(0), l1);
         // the preopen is never freed, so nothing is handed out as 0
         inst.fs_drop(0);
         assert_eq!(inst.fs_stat(0), 3);
         assert_eq!(inst.fs_open({SUB}, 0b10, 1), 0);
         assert_ne!(inst.fs_last(), 0);"
    );
    assert_succeeded(&run(Some(&root), &body));
}
