//! What a descriptor of the typed host stays bound to: the file it opened (not the path
//! that names it later), the preopen (a moved directory is refused), and its own stream
//! positions. Each export of `CORE` returns 0 for success and `100 + the error-code` for
//! an error, so a test reads `100 + 31` as `not-permitted`.

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
  resource output-stream {
    write: func(contents: list<u8>) -> result<_, stream-error>;
  }
}
"#;

const FILESYSTEM: &str = r#"
package wasi:filesystem@0.2.0;
interface types {
  use wasi:io/streams@0.2.0.{input-stream, output-stream, error};
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
    write-via-stream: func(offset: filesize) -> result<output-stream, error-code>;
    append-via-stream: func() -> result<output-stream, error-code>;
    open-at: func(path-flags: path-flags, path: string, open-flags: open-flags, %flags: descriptor-flags) -> result<descriptor, error-code>;
    set-size: func(size: filesize) -> result<_, error-code>;
    sync: func() -> result<_, error-code>;
    rename-at: func(old-path: string, new-descriptor: borrow<descriptor>, new-path: string) -> result<_, error-code>;
    set-times: func(data-access-timestamp: new-timestamp, data-modification-timestamp: new-timestamp) -> result<_, error-code>;
    stat: func() -> result<descriptor-stat, error-code>;
    metadata-hash: func() -> result<metadata-hash-value, error-code>;
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
  (import "wasi:io/streams@0.2.0" "[resource-drop]input-stream" (func (param i32)))
  (import "wasi:io/streams@0.2.0" "[resource-drop]output-stream" (func (param i32)))
  (import "wasi:io/streams@0.2.0" "[method]input-stream.read" (func $read (param i32 i64 i32)))
  (import "wasi:io/streams@0.2.0" "[method]output-stream.write" (func $write (param i32 i32 i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.read-via-stream" (func $rvs (param i32 i64 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.write-via-stream" (func $wvs (param i32 i64 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.append-via-stream" (func $avs (param i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.open-at" (func $open (param i32 i32 i32 i32 i32 i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.set-size" (func $set_size (param i32 i64 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.sync" (func $sync (param i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.rename-at" (func $rename (param i32 i32 i32 i32 i32 i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.set-times" (func $set_times (param i32 i32 i64 i32 i32 i64 i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.stat" (func $stat (param i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.metadata-hash" (func $hash (param i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.create-directory-at" (func $mkdir (param i32 i32 i32 i32 )))
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
  (data (i32.const 416) "g.txt")
  (data (i32.const 432) "w.txt")
  (data (i32.const 448) "x")
  (data (i32.const 464) "d")
  (data (i32.const 480) "d/sub")
  (data (i32.const 496) "e")
  (data (i32.const 512) "abCDefGHZZ")

  ;; a `result<_, error-code>` at `at`: 0, or 100 + the error code
  (func $status (param $at i32) (result i32)
    (if (result i32) (i32.load8_u (local.get $at))
      (then (i32.add (i32.const 100) (i32.load8_u (i32.add (local.get $at) (i32.const 1)))))
      (else (i32.const 0))))

  ;; the handle of the preopen is 0
  ;; open-at under descriptor `dir`: 0 (the handle is `fs_last`) or 100 + the error code
  (func (export "fs_open") (param $dir i32) (param $p i32) (param $n i32) (param $o i32) (param $d i32) (result i32)
    (call $open (local.get $dir) (i32.const 1) (local.get $p) (local.get $n)
                (local.get $o) (local.get $d) (i32.const 600))
    (if (result i32) (i32.load8_u (i32.const 600))
      (then (i32.add (i32.const 100) (i32.load8_u (i32.const 604))))
      (else (global.set $last (i32.load (i32.const 604))) (i32.const 0))))
  (func (export "fs_last") (result i32) (global.get $last))
  (func (export "fs_mkdir") (param $dir i32) (param $p i32) (param $n i32) (result i32)
    (call $mkdir (local.get $dir) (local.get $p) (local.get $n) (i32.const 700))
    (call $status (i32.const 700)))
  (func (export "fs_rmdir") (param $dir i32) (param $p i32) (param $n i32) (result i32)
    (call $rmdir (local.get $dir) (local.get $p) (local.get $n) (i32.const 700))
    (call $status (i32.const 700)))
  (func (export "fs_unlink") (param $dir i32) (param $p i32) (param $n i32) (result i32)
    (call $unlink (local.get $dir) (local.get $p) (local.get $n) (i32.const 700))
    (call $status (i32.const 700)))
  ;; rename-at from descriptor `dir` to (p2, n2) under descriptor `nd`
  (func (export "fs_rename") (param $dir i32) (param $p i32) (param $n i32) (param $nd i32) (param $p2 i32) (param $n2 i32) (result i32)
    (call $rename (local.get $dir) (local.get $p) (local.get $n) (local.get $nd) (local.get $p2) (local.get $n2) (i32.const 700))
    (call $status (i32.const 700)))
  (func (export "fs_sync") (param $h i32) (result i32)
    (call $sync (local.get $h) (i32.const 700))
    (call $status (i32.const 700)))
  ;; both times are set to the timestamp 1000000000 s
  (func (export "fs_set_times") (param $h i32) (result i32)
    (call $set_times (local.get $h) (i32.const 2) (i64.const 1000000000) (i32.const 0)
                     (i32.const 2) (i64.const 1000000000) (i32.const 0) (i32.const 700))
    (call $status (i32.const 700)))
  (func (export "fs_set_size") (param $h i32) (param $size i64) (result i32)
    (call $set_size (local.get $h) (local.get $size) (i32.const 700))
    (call $status (i32.const 700)))
  ;; the size of the file behind the descriptor, or -1
  (func (export "fs_size") (param $h i32) (result i64)
    (call $stat (local.get $h) (i32.const 640))
    (if (result i64) (i32.load8_u (i32.const 640))
      (then (i64.const -1))
      (else (i64.load (i32.const 664)))))
  ;; the lower half of the metadata hash; an error traps, so it cannot pass for a hash
  (func (export "fs_hash") (param $h i32) (result i64)
    (call $hash (local.get $h) (i32.const 720))
    (if (result i64) (i32.load8_u (i32.const 720)) (then unreachable) (else (i64.load (i32.const 728)))))
  ;; the three streams: the stream handle, or 1000 + the error code
  (func (export "fs_rstream") (param $h i32) (param $offset i64) (result i32)
    (call $rvs (local.get $h) (local.get $offset) (i32.const 740))
    (if (result i32) (i32.load8_u (i32.const 740))
      (then (i32.add (i32.const 1000) (i32.load8_u (i32.const 744))))
      (else (i32.load (i32.const 744)))))
  (func (export "fs_wstream") (param $h i32) (param $offset i64) (result i32)
    (call $wvs (local.get $h) (local.get $offset) (i32.const 740))
    (if (result i32) (i32.load8_u (i32.const 740))
      (then (i32.add (i32.const 1000) (i32.load8_u (i32.const 744))))
      (else (i32.load (i32.const 744)))))
  (func (export "fs_astream") (param $h i32) (result i32)
    (call $avs (local.get $h) (i32.const 740))
    (if (result i32) (i32.load8_u (i32.const 740))
      (then (i32.add (i32.const 1000) (i32.load8_u (i32.const 744))))
      (else (i32.load (i32.const 744)))))
  ;; read: the length of what was read (kept for `in_byte`), or -1
  (func (export "in_read") (param $s i32) (param $len i64) (result i32)
    (call $read (local.get $s) (local.get $len) (i32.const 760))
    (if (result i32) (i32.load8_u (i32.const 760))
      (then (i32.const -1))
      (else (i32.load (i32.const 768)))))
  (func (export "in_byte") (param $i i32) (result i32)
    (i32.load8_u (i32.add (i32.load (i32.const 764)) (local.get $i))))
  ;; write the bytes at (p, n): 0, or 1 for an error
  (func (export "out_write") (param $s i32) (param $p i32) (param $n i32) (result i32)
    (call $write (local.get $s) (local.get $p) (local.get $n) (i32.const 780))
    (i32.load8_u (i32.const 780)))
  (func (export "fs_drop") (param i32) (call $drop_fd (local.get 0))))
"#;

// the strings of CORE
const F: &str = "400, 5";
const G: &str = "416, 5";
const W: &str = "432, 5";
const X: &str = "448, 1";
const D: &str = "464, 1";
const DSUB: &str = "480, 5";
const E: &str = "496, 1";
// "abCDefGHZZ": the bytes of the writes of the streams test, two at a time
const AB: &str = "512, 2";
const CD: &str = "514, 2";
const EF: &str = "516, 2";
const GH: &str = "518, 2";
const ZZ: &str = "520, 2";

// open-flags: create 1, directory 2; descriptor-flags: read 1, write 2
// error codes: is-directory 14, not-directory 24, not-permitted 31

/// Runs `body` with `inst` on a host whose writable preopen is `root`.
fn run(root: &std::path::Path, body: &str) -> std::process::Output {
    run_program(
        &component_from_wit(&[CLOCKS, IO, FILESYSTEM], WORLD, "w"),
        CORE,
        &format!(
            "WasiHost::new(Vec::new()).with_writable_preopen(std::path::PathBuf::from({:?}))",
            root.to_str().unwrap()
        ),
        body,
    )
}

#[cfg(unix)]
#[test]
fn a_directory_descriptor_whose_path_was_moved_out_of_the_preopen_is_refused() {
    let root = fresh_root("identity_moved");
    let outside = root.parent().unwrap().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::create_dir(root.join("e")).unwrap();
    std::os::unix::fs::symlink(&outside, root.join("e/sub")).unwrap();
    let before = std::fs::metadata(&outside).unwrap().modified().unwrap();
    let body = format!(
        "// open the directory d/sub, then make `d` the directory `e`, so the path the
         // descriptor holds leads through a symlink to outside the root
         assert_eq!(inst.fs_mkdir(0, {D}), 0);
         assert_eq!(inst.fs_mkdir(0, {DSUB}), 0);
         assert_eq!(inst.fs_open(0, {DSUB}, 2, 1), 0);
         let sub = inst.fs_last();
         assert_eq!(inst.fs_rmdir(0, {DSUB}), 0);
         assert_eq!(inst.fs_rmdir(0, {D}), 0);
         assert_eq!(inst.fs_rename(0, {E}, 0, {D}), 0);
         assert_eq!(inst.fs_sync(sub), 100 + 31);
         assert_eq!(inst.fs_set_times(sub), 100 + 31);"
    );
    assert_succeeded(&run(&root, &body));
    let after = std::fs::metadata(&outside).unwrap().modified().unwrap();
    assert_eq!(after, before, "changed a directory outside the root");
}

#[cfg(unix)]
#[test]
fn a_descriptor_stays_on_the_file_it_opened_when_its_path_is_reused() {
    let root = fresh_root("identity_reused");
    std::fs::write(root.join("f.txt"), "0123456789").unwrap();
    let write_only = root.join("w.txt");
    std::fs::write(&write_only, "w").unwrap();
    std::fs::set_permissions(
        &write_only,
        std::os::unix::fs::PermissionsExt::from_mode(0o200),
    )
    .unwrap();
    let body = format!(
        "assert_eq!(inst.fs_open(0, {F}, 0, 3), 0);
         let old = inst.fs_last();
         assert_eq!(inst.fs_unlink(0, {F}), 0);
         assert_eq!(inst.fs_open(0, {F}, 1, 3), 0);
         let new = inst.fs_last();
         // resizing the first descriptor must not touch the file the path names now
         assert_eq!(inst.fs_set_size(old, 5), 0);
         assert_eq!(inst.fs_size(old), 5);
         assert_eq!(inst.fs_size(new), 0);
         // two files are not the same object, though they were opened under one path
         assert_ne!(inst.fs_hash(old), inst.fs_hash(new));
         // a descriptor for writing only opens a file that cannot be read
         assert_eq!(inst.fs_open(0, {W}, 0, 2), 0);"
    );
    assert_succeeded(&run(&root, &body));
    assert_eq!(std::fs::metadata(root.join("f.txt")).unwrap().len(), 0);
}

// needs POSIX semantics: a file unlinked or replaced while open stays reachable
#[cfg(unix)]
#[test]
fn a_file_descriptor_is_not_a_directory_when_a_directory_takes_its_path() {
    let root = fresh_root("identity_not_dir");
    std::fs::write(root.join("f.txt"), "x").unwrap();
    let body = format!(
        "assert_eq!(inst.fs_open(0, {F}, 0, 3), 0);
         let old = inst.fs_last();
         assert_eq!(inst.fs_unlink(0, {F}), 0);
         assert_eq!(inst.fs_mkdir(0, {F}), 0);
         // not-directory 24, not whatever `x` is under the directory that has the path now
         assert_eq!(inst.fs_open(old, {X}, 0, 1), 100 + 24);"
    );
    assert_succeeded(&run(&root, &body));
}

#[test]
fn streams_over_one_descriptor_keep_their_own_positions_and_outlive_it() {
    let root = fresh_root("identity_streams");
    std::fs::write(root.join("f.txt"), "0123456789").unwrap();
    let body = format!(
        "macro_rules! read {{
             ($s:expr, $n:expr) => {{{{
                 let n = inst.in_read($s, $n);
                 assert!(n >= 0);
                 (0..n).map(|i| inst.in_byte(i) as u8).collect::<Vec<u8>>()
             }}}};
         }}
         assert_eq!(inst.fs_open(0, {F}, 0, 3), 0);
         let f = inst.fs_last();
         // two input streams on one descriptor keep their own positions
         let a = inst.fs_rstream(f, 0);
         let b = inst.fs_rstream(f, 5);
         assert_eq!(read!(a, 2), b\"01\");
         assert_eq!(read!(b, 2), b\"56\");
         assert_eq!(read!(a, 2), b\"23\");
         assert_eq!(read!(b, 2), b\"78\");
         // a stream writing from offset 2 and an appending one, used in turn
         let w = inst.fs_wstream(f, 2);
         let p = inst.fs_astream(f);
         assert_eq!(inst.out_write(w, {AB}), 0);
         assert_eq!(inst.out_write(p, {CD}), 0);
         assert_eq!(inst.out_write(w, {EF}), 0);
         assert_eq!(inst.out_write(p, {GH}), 0);
         // another stream sees all of it
         let r = inst.fs_rstream(f, 0);
         assert_eq!(read!(r, 100), b\"01abef6789CDGH\");
         // the streams outlive the descriptor
         let r2 = inst.fs_rstream(f, 6);
         inst.fs_drop(f);
         assert_eq!(inst.out_write(w, {ZZ}), 0);
         assert_eq!(read!(r2, 2), b\"ZZ\");"
    );
    assert_succeeded(&run(&root, &body));
    assert_eq!(
        std::fs::read_to_string(root.join("f.txt")).unwrap(),
        "01abefZZ89CDGH"
    );
}

// needs POSIX semantics: a file unlinked or replaced while open stays reachable
#[cfg(unix)]
#[test]
fn a_descriptor_stays_on_its_file_when_another_file_is_renamed_over_its_path() {
    let root = fresh_root("identity_renamed");
    std::fs::write(root.join("f.txt"), "0123456789").unwrap();
    std::fs::write(root.join("g.txt"), "GGGGGGGGGGGGGG").unwrap();
    let body = format!(
        "assert_eq!(inst.fs_open(0, {F}, 0, 1), 0);
         let old = inst.fs_last();
         // another file takes the name `f.txt`
         assert_eq!(inst.fs_rename(0, {G}, 0, {F}), 0);
         // the descriptor still has the 10-byte file it opened, not the 14-byte one
         assert_eq!(inst.fs_size(old), 10);
         let s = inst.fs_rstream(old, 0);
         assert_eq!(inst.in_read(s, 2), 2);
         assert_eq!((inst.in_byte(0), inst.in_byte(1)), (b'0' as i32, b'1' as i32));"
    );
    assert_succeeded(&run(&root, &body));
    assert_eq!(
        std::fs::read_to_string(root.join("f.txt")).unwrap(),
        "GGGGGGGGGGGGGG"
    );
}
