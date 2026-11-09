//! `stat-at` and `metadata-hash-at` on the typed host honor `symlink-follow`: with it they
//! describe the target, without it the link itself. The imports are declared in WIT text;
//! errors are read as `100 + the error-code byte`.

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
  flags path-flags { symlink-follow }
  flags open-flags { create, directory, exclusive, truncate }
  flags descriptor-flags {
    read, write, file-integrity-sync, data-integrity-sync, requested-write-sync, mutate-directory
  }
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
    open-at: func(path-flags: path-flags, path: string, open-flags: open-flags, %flags: descriptor-flags) -> result<descriptor, error-code>;
    stat-at: func(path-flags: path-flags, path: string) -> result<descriptor-stat, error-code>;
    metadata-hash-at: func(path-flags: path-flags, path: string) -> result<metadata-hash-value, error-code>;
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
  (import "wasi:io/streams@0.2.0" "[method]input-stream.read" (func (param i32 i64 i32)))
  (import "wasi:io/streams@0.2.0" "[resource-drop]input-stream" (func (param i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.stat-at" (func $stat_at (param i32 i32 i32 i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.open-at" (func $open (param i32 i32 i32 i32 i32 i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.metadata-hash-at" (func $hash_at (param i32 i32 i32 i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[resource-drop]descriptor" (func (param i32)))
  (import "wasi:filesystem/preopens@0.2.0" "get-directories" (func $dirs (param i32)))
  (memory (export "memory") 1)
  (global $brk (mut i32) (i32.const 1024))
  (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32) (local $p i32)
    (local.set $p (i32.and (i32.add (global.get $brk) (i32.sub (local.get 2) (i32.const 1)))
                           (i32.sub (i32.const 0) (local.get 2))))
    (global.set $brk (i32.add (local.get $p) (local.get 3)))
    (local.get $p))
  (data (i32.const 300) "tgt.txt")
  (data (i32.const 320) "ln")
  (data (i32.const 330) "out")
  (data (i32.const 340) "nope")
  (data (i32.const 350) "../secret.txt")
  (data (i32.const 370) "dangling")
  (data (i32.const 390) ".")
  (data (i32.const 400) "sub")
  (data (i32.const 410) "sub/")
  (data (i32.const 420) "sub/.")
  (data (i32.const 440) "dirln/in.txt")
  (data (i32.const 460) "dirln")

  (func $root (result i32)
    (call $dirs (i32.const 100))
    (i32.load (i32.load (i32.const 100))))
  (func $code (param $at i32) (result i32)
    (i32.add (i32.const 100) (i32.load8_u (local.get $at))))

  ;; stat-at with path flags `f`: the type byte, or 100 + the error code
  (func (export "fs_stat_at") (param $f i32) (param $p i32) (param $n i32) (result i32)
    (call $stat_at (call $root) (local.get $f) (local.get $p) (local.get $n) (i32.const 600))
    (if (result i32) (i32.load8_u (i32.const 600))
      (then (call $code (i32.const 608)))
      (else (i32.load8_u (i32.const 608)))))
  ;; open-at from the preopen with path flags `f`, open flags `o` (1 create, 2 directory) and
  ;; descriptor flags 1 (read): 0 or 100 + the error code
  (func (export "fs_open") (param $f i32) (param $p i32) (param $n i32) (param $o i32) (result i32)
    (call $open (call $root) (local.get $f) (local.get $p) (local.get $n)
                (local.get $o) (i32.const 1) (i32.const 800))
    (if (result i32) (i32.load8_u (i32.const 800))
      (then (call $code (i32.const 804)))
      (else (i32.const 0))))
  ;; the size of the last stat
  (func (export "fs_size") (result i64) (i64.load (i32.const 624)))

  ;; the low half of the hash with path flags `f`; 0 on error
  (func (export "fs_hash_at") (param $f i32) (param $p i32) (param $n i32) (result i64)
    (call $hash_at (call $root) (local.get $f) (local.get $p) (local.get $n) (i32.const 720))
    (if (result i64) (i32.load8_u (i32.const 720)) (then (i64.const 0)) (else (i64.load (i32.const 728)))))
  (func (export "fs_hash_at_code") (param $f i32) (param $p i32) (param $n i32) (result i32)
    (call $hash_at (call $root) (local.get $f) (local.get $p) (local.get $n) (i32.const 720))
    (if (result i32) (i32.load8_u (i32.const 720)) (then (call $code (i32.const 728))) (else (i32.const 0)))))
"#;

#[cfg(unix)]
const TGT: &str = "300, 7";
#[cfg(unix)]
const LN: &str = "320, 2";
#[cfg(unix)]
const OUT: &str = "330, 3";
const NOPE: &str = "340, 4";
const PARENT_SECRET: &str = "350, 13";
#[cfg(unix)]
const DANGLING: &str = "370, 8";
const DOT: &str = "390, 1";
const SUB: &str = "400, 3";
const SUB_SLASH: &str = "410, 4";
const SUB_SLASH_DOT: &str = "420, 5";
const EMPTY: &str = "0, 0";
#[cfg(unix)]
const VIA_DIR_LINK: &str = "440, 12";
#[cfg(unix)]
const DIR_LINK: &str = "460, 5";

fn run(root: &Path, body: &str) -> std::process::Output {
    run_with(root, "with_preopen", body)
}

fn run_with(root: &Path, preopen: &str, body: &str) -> std::process::Output {
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

#[cfg(unix)]
#[test]
fn stat_at_reports_the_link_itself_without_symlink_follow_and_the_target_with_it() {
    let root = fresh_root("nofollow_stat");
    std::fs::write(root.join("tgt.txt"), "twelve bytes").unwrap();
    std::os::unix::fs::symlink("tgt.txt", root.join("ln")).unwrap();
    let body = format!(
        "// symbolic-link 5 without the flag, regular-file 6 with it (the target's size)
         assert_eq!(inst.fs_stat_at(0, {LN}), 5);
         assert_eq!(inst.fs_stat_at(1, {LN}), 6);
         assert_eq!(inst.fs_size(), 12);
         // a plain file is a regular file either way
         assert_eq!(inst.fs_stat_at(0, {TGT}), 6);
         assert_eq!(inst.fs_stat_at(1, {TGT}), 6);"
    );
    assert_succeeded(&run(&root, &body));
}

#[cfg(unix)]
#[test]
fn metadata_hash_at_hashes_the_link_itself_without_symlink_follow() {
    let root = fresh_root("nofollow_hash");
    std::fs::write(root.join("tgt.txt"), "x").unwrap();
    std::os::unix::fs::symlink("tgt.txt", root.join("ln")).unwrap();
    let body = format!(
        "let target = inst.fs_hash_at(0, {TGT});
         let link = inst.fs_hash_at(0, {LN});
         assert!(target != 0 && link != 0);
         // the link has an identity of its own; following it gives the target's
         assert_ne!(link, target);
         assert_eq!(inst.fs_hash_at(1, {LN}), target);
         assert_eq!(inst.fs_hash_at(1, {TGT}), target);"
    );
    assert_succeeded(&run(&root, &body));
}

#[cfg(unix)]
#[test]
fn a_link_leading_out_of_the_preopen_is_described_without_following_and_refused_with_it() {
    let root = fresh_root("nofollow_out");
    let secret = root.parent().unwrap().join("secret.txt");
    std::os::unix::fs::symlink(&secret, root.join("out")).unwrap();
    std::os::unix::fs::symlink("gone-for-good", root.join("dangling")).unwrap();
    let body = format!(
        "// never followed, so nothing leaves the preopen: the link itself is reported
         assert_eq!(inst.fs_stat_at(0, {OUT}), 5);
         assert_ne!(inst.fs_hash_at(0, {OUT}), 0);
         assert_eq!(inst.fs_stat_at(0, {DANGLING}), 5);
         assert_ne!(inst.fs_hash_at(0, {DANGLING}), 0);
         // followed, it leads out: not-permitted 31
         assert_eq!(inst.fs_stat_at(1, {OUT}), 100 + 31);
         assert_eq!(inst.fs_hash_at_code(1, {OUT}), 100 + 31);"
    );
    assert_succeeded(&run(&root, &body));
}

#[test]
fn escaping_by_dot_dot_stays_not_permitted_and_a_missing_path_is_no_entry_either_way() {
    let root = fresh_root("nofollow_edges");
    let body = format!(
        "for flags in [0, 1] {{
             assert_eq!(inst.fs_stat_at(flags, {PARENT_SECRET}), 100 + 31);
             assert_eq!(inst.fs_hash_at_code(flags, {PARENT_SECRET}), 100 + 31);
             assert_eq!(inst.fs_stat_at(flags, {NOPE}), 100 + 20);
             assert_eq!(inst.fs_hash_at_code(flags, {NOPE}), 100 + 20);
         }}"
    );
    assert_succeeded(&run(&root, &body));
}

#[test]
fn paths_naming_a_directory_itself_are_resolved_even_without_symlink_follow() {
    let root = fresh_root("nofollow_dir");
    std::fs::create_dir(root.join("sub")).unwrap();
    let body = format!(
        "// `.`, the empty path and a trailing `/` or `/.` name the directory itself:
         // directory 3, and the directory's own hash, whatever the flags
         let sub = inst.fs_hash_at(0, {SUB});
         let dot = inst.fs_hash_at(1, {DOT});
         assert!(sub != 0 && dot != 0 && sub != dot);
         for flags in [0, 1] {{
             assert_eq!(inst.fs_stat_at(flags, {DOT}), 3);
             assert_eq!(inst.fs_stat_at(flags, {EMPTY}), 3);
             assert_eq!(inst.fs_stat_at(flags, {SUB}), 3);
             assert_eq!(inst.fs_stat_at(flags, {SUB_SLASH}), 3);
             assert_eq!(inst.fs_stat_at(flags, {SUB_SLASH_DOT}), 3);
             assert_eq!(inst.fs_hash_at(flags, {DOT}), dot);
             assert_eq!(inst.fs_hash_at(flags, {EMPTY}), dot);
             assert_eq!(inst.fs_hash_at(flags, {SUB}), sub);
             assert_eq!(inst.fs_hash_at(flags, {SUB_SLASH}), sub);
             assert_eq!(inst.fs_hash_at(flags, {SUB_SLASH_DOT}), sub);
         }}"
    );
    assert_succeeded(&run(&root, &body));
}

#[cfg(unix)]
#[test]
fn open_at_refuses_a_final_symlink_without_symlink_follow_as_loop_and_follows_it_with_the_flag() {
    let root = fresh_root("nofollow_open");
    std::fs::write(root.join("tgt.txt"), "twelve bytes").unwrap();
    std::fs::create_dir(root.join("sub")).unwrap();
    std::os::unix::fs::symlink("tgt.txt", root.join("ln")).unwrap();
    std::os::unix::fs::symlink("gone-for-good", root.join("dangling")).unwrap();
    let body = format!(
        "// no symlink-follow: a link as the last component is loop 15 (ELOOP), even dangling
         assert_eq!(inst.fs_open(0, {LN}, 0), 100 + 15);
         assert_eq!(inst.fs_open(0, {DANGLING}, 0), 100 + 15);
         // with it the link is expanded; a dangling one is no-entry 20
         assert_eq!(inst.fs_open(1, {LN}, 0), 0);
         assert_eq!(inst.fs_open(1, {DANGLING}, 0), 100 + 20);
         // plain files and directories open either way
         assert_eq!(inst.fs_open(0, {TGT}, 0), 0);
         assert_eq!(inst.fs_open(0, {SUB}, 2), 0);
         assert_eq!(inst.fs_open(0, {NOPE}, 0), 100 + 20);"
    );
    assert_succeeded(&run(&root, &body));
}

#[cfg(unix)]
#[test]
fn open_at_nofollow_affects_only_the_last_component_and_the_preopen_still_confines() {
    let root = fresh_root("nofollow_open_parent");
    std::fs::create_dir(root.join("sub")).unwrap();
    std::fs::write(root.join("sub").join("in.txt"), "x").unwrap();
    std::os::unix::fs::symlink("sub", root.join("dirln")).unwrap();
    let secret = root.parent().unwrap().join("secret.txt");
    std::os::unix::fs::symlink(&secret, root.join("out")).unwrap();
    let body = format!(
        "// a link in the middle of the path is followed whatever the flags
         assert_eq!(inst.fs_open(0, {VIA_DIR_LINK}, 0), 0);
         assert_eq!(inst.fs_open(1, {VIA_DIR_LINK}, 0), 0);
         // a link as the last component, naming a directory: loop without the flag
         assert_eq!(inst.fs_open(0, {DIR_LINK}, 0), 100 + 15);
         assert_eq!(inst.fs_open(1, {DIR_LINK}, 2), 0);
         // a link out of the preopen: loop without the flag, not-permitted 31 with it
         assert_eq!(inst.fs_open(0, {OUT}, 0), 100 + 15);
         assert_eq!(inst.fs_open(1, {OUT}, 0), 100 + 31);
         // a path naming a directory itself needs no flag
         for flags in [0, 1] {{
             assert_eq!(inst.fs_open(flags, {DOT}, 2), 0);
             assert_eq!(inst.fs_open(flags, {EMPTY}, 2), 0);
             assert_eq!(inst.fs_open(flags, {SUB_SLASH}, 2), 0);
             assert_eq!(inst.fs_open(flags, {SUB_SLASH_DOT}, 2), 0);
         }}"
    );
    assert_succeeded(&run(&root, &body));
}

#[cfg(unix)]
#[test]
fn open_at_create_through_a_symlink_name_is_loop_without_symlink_follow() {
    let root = fresh_root("nofollow_open_create");
    std::fs::write(root.join("tgt.txt"), "twelve bytes").unwrap();
    std::os::unix::fs::symlink("tgt.txt", root.join("ln")).unwrap();
    std::os::unix::fs::symlink("gone-for-good", root.join("dangling")).unwrap();
    let secret = root.parent().unwrap().join("secret.txt");
    std::os::unix::fs::symlink(&secret, root.join("out")).unwrap();
    let body = format!(
        "// O_CREAT | O_NOFOLLOW on a symlink is ELOOP, existing target or dangling
         assert_eq!(inst.fs_open(0, {LN}, 1), 100 + 15);
         assert_eq!(inst.fs_open(0, {DANGLING}, 1), 100 + 15);
         // with symlink-follow nothing changes: the target opens; a dangling link stays
         // not-permitted 31 (never created through)
         assert_eq!(inst.fs_open(1, {LN}, 1), 0);
         assert_eq!(inst.fs_open(1, {DANGLING}, 1), 100 + 31);
         // create + exclusive never follows a link, whatever the flags: the name exists,
         // exist 7 (before loop), dangling, leading out of the preopen or not
         for flags in [0, 1] {{
             assert_eq!(inst.fs_open(flags, {LN}, 5), 100 + 7);
             assert_eq!(inst.fs_open(flags, {DANGLING}, 5), 100 + 7);
             assert_eq!(inst.fs_open(flags, {OUT}, 5), 100 + 7);
         }}
         // a new name is created either way
         assert_eq!(inst.fs_open(0, {NOPE}, 1), 0);"
    );
    assert_succeeded(&run_with(&root, "with_writable_preopen", &body));
    assert!(!root.join("gone-for-good").exists());
}
