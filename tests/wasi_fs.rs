//! Integration tests for the WASI filesystem subset: `path_open`,
//! `fd_prestat_get`, `fd_prestat_dir_name`, and the file-backed variants of
//! `fd_read`/`fd_write`/`fd_seek`/`fd_close`.
//!
//! A module that imports any of the preopen/`path_open` functions gains a real
//! file-descriptor table on its `Instance` and routes descriptors >= 4 to
//! `std::fs::File`s opened *within a single preopened directory* (fd 3, name
//! ".", the process's current directory). Paths that are absolute or escape the
//! preopen via ".." are rejected (ENOTCAPABLE). The generated Rust is compiled
//! with `rustc -D warnings` and run as a real child process against real files
//! in a temp directory (no mocking).

use std::process::Command;

/// Compile the transpiled module plus a trailing `extra` (`fn main`) block,
/// returning both the binary path and the temp directory it lives in (used as
/// the child's working directory so `path_open` resolves against real files).
fn compile(test: &str, wat: &str, extra: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let wasm = wat::parse_str(wat).expect("valid wat");
    let generated = wasm2rs::transpile(&wasm).expect("transpile ok");

    let dir = std::env::temp_dir().join(format!("wasm2rs_wasifs_{test}"));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let src = dir.join("gen.rs");
    let bin = dir.join(if cfg!(windows) { "gen.exe" } else { "gen" });

    let program = format!("{generated}\n{extra}\n");
    std::fs::write(&src, &program).expect("write generated source");

    let out = Command::new("rustc")
        .current_dir(&dir)
        .arg(&src)
        .arg("--edition")
        .arg("2021")
        .arg("-D")
        .arg("warnings")
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("run rustc");
    assert!(
        out.status.success(),
        "generated code failed to compile:\n{program}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&out.stderr),
    );
    (bin, dir)
}

// fd_prestat_get/fd_prestat_dir_name advertise exactly one preopened directory
// (fd 3, name "."). `run` writes fd 3's prestat at 0 and its name at 16, then
// returns the errno of probing fd 4 (which must be EBADF so libc stops).
const PRESTAT: &str = r#"
    (module
      (import "wasi_snapshot_preview1" "fd_prestat_get"
        (func $pg (param i32 i32) (result i32)))
      (import "wasi_snapshot_preview1" "fd_prestat_dir_name"
        (func $pn (param i32 i32 i32) (result i32)))
      (memory 1)
      (func (export "run") (result i32)
        (drop (call $pg (i32.const 3) (i32.const 0)))
        (drop (call $pn (i32.const 3) (i32.const 16) (i32.const 8)))
        (call $pg (i32.const 4) (i32.const 32))))
    "#;

#[test]
fn fd_prestat_advertises_one_preopen_dir() {
    let extra = "\
fn main() {
    let mut i = Instance::new();
    // Probing fd 4 must report EBADF so wasi-libc stops scanning preopens.
    assert_eq!(i.func2(), 8);
    assert_eq!(i.mem()[0], 0, \"fd 3 should be a preopen directory (tag 0)\");
    let name_len = u32::from_le_bytes([i.mem()[4], i.mem()[5], i.mem()[6], i.mem()[7]]);
    assert_eq!(name_len, 1, \"the preopen name \\\".\\\" is one byte\");
    assert_eq!(i.mem()[16], b'.', \"the preopen dir name is \\\".\\\"\");
}
";
    let (bin, dir) = compile("prestat", PRESTAT, extra);
    let status = Command::new(&bin)
        .current_dir(&dir)
        .status()
        .expect("run generated binary");
    assert!(status.success(), "prestat assertions failed");
}

// path_open opens a real file in the preopen dir and fd_read reads it. The path
// "input.txt" lives at offset 100; an iovec at 0 points at offset 200 (cap 64);
// the opened fd is stored at 300. `open_and_read` returns the read errno and
// stores the byte count at offset 8.
const OPEN_READ: &str = r#"
    (module
      (import "wasi_snapshot_preview1" "path_open"
        (func $po (param i32 i32 i32 i32 i32 i64 i64 i32 i32) (result i32)))
      (import "wasi_snapshot_preview1" "fd_read"
        (func $rd (param i32 i32 i32 i32) (result i32)))
      (memory 1)
      (data (i32.const 100) "input.txt")
      (data (i32.const 0) "\c8\00\00\00\40\00\00\00")
      (func (export "open_and_read") (result i32)
        (drop (call $po (i32.const 3) (i32.const 0) (i32.const 100) (i32.const 9)
                        (i32.const 0) (i64.const 2) (i64.const 0) (i32.const 0) (i32.const 300)))
        (call $rd (i32.load (i32.const 300)) (i32.const 0) (i32.const 1) (i32.const 8))))
    "#;

#[test]
fn path_open_then_fd_read_reads_a_real_file() {
    let extra = "\
fn main() {
    let mut i = Instance::new();
    let errno = i.func2();
    assert_eq!(errno, 0, \"read should succeed\");
    // The freshly opened file gets the first free descriptor, 4.
    let fd = u32::from_le_bytes([i.mem()[300], i.mem()[301], i.mem()[302], i.mem()[303]]);
    assert_eq!(fd, 4, \"first opened file descriptor should be 4\");
    let n = u32::from_le_bytes([i.mem()[8], i.mem()[9], i.mem()[10], i.mem()[11]]) as usize;
    assert_eq!(&i.mem()[200..200 + n], b\"file-contents-123\\n\");
}
";
    let (bin, dir) = compile("open_read", OPEN_READ, extra);
    std::fs::write(dir.join("input.txt"), b"file-contents-123\n").expect("write input file");
    let status = Command::new(&bin)
        .current_dir(&dir)
        .status()
        .expect("run generated binary");
    assert!(status.success(), "path_open+fd_read assertions failed");
}

// path_open with O_CREAT|O_TRUNC creates a file, fd_write persists bytes, and
// fd_close releases it. "out.txt" at 100; iovec at 0 -> offset 200 (len 9);
// the 9 bytes "persisted" at 200. `run` returns the fd_close errno.
const CREATE_WRITE: &str = r#"
    (module
      (import "wasi_snapshot_preview1" "path_open"
        (func $po (param i32 i32 i32 i32 i32 i64 i64 i32 i32) (result i32)))
      (import "wasi_snapshot_preview1" "fd_write"
        (func $wr (param i32 i32 i32 i32) (result i32)))
      (import "wasi_snapshot_preview1" "fd_close"
        (func $cl (param i32) (result i32)))
      (memory 1)
      (data (i32.const 100) "out.txt")
      (data (i32.const 0) "\c8\00\00\00\09\00\00\00")
      (data (i32.const 200) "persisted")
      (func (export "run") (result i32)
        (drop (call $po (i32.const 3) (i32.const 0) (i32.const 100) (i32.const 7)
                        (i32.const 9) (i64.const 64) (i64.const 0) (i32.const 0) (i32.const 300)))
        (drop (call $wr (i32.load (i32.const 300)) (i32.const 0) (i32.const 1) (i32.const 308)))
        (call $cl (i32.load (i32.const 300)))))
    "#;

#[test]
fn path_open_create_then_fd_write_persists_to_disk() {
    let extra = "\
fn main() {
    let mut i = Instance::new();
    assert_eq!(i.func3(), 0, \"close should succeed\");
}
";
    let (bin, dir) = compile("create_write", CREATE_WRITE, extra);
    let out_path = dir.join("out.txt");
    let _ = std::fs::remove_file(&out_path);
    let status = Command::new(&bin)
        .current_dir(&dir)
        .status()
        .expect("run generated binary");
    assert!(status.success(), "create/write/close failed");
    let written = std::fs::read(&out_path).expect("output file should exist");
    assert_eq!(written, b"persisted");
}

// fd_filestat_get reports an opened file's size and type. Opens "input.txt"
// then writes its 64-byte filestat at offset 400; `run` returns the errno.
const FILESTAT: &str = r#"
    (module
      (import "wasi_snapshot_preview1" "path_open"
        (func $po (param i32 i32 i32 i32 i32 i64 i64 i32 i32) (result i32)))
      (import "wasi_snapshot_preview1" "fd_filestat_get"
        (func $fs (param i32 i32) (result i32)))
      (memory 1)
      (data (i32.const 100) "input.txt")
      (func (export "run") (result i32)
        (drop (call $po (i32.const 3) (i32.const 0) (i32.const 100) (i32.const 9)
                        (i32.const 0) (i64.const 2) (i64.const 0) (i32.const 0) (i32.const 300)))
        (call $fs (i32.load (i32.const 300)) (i32.const 400))))
    "#;

#[test]
fn fd_filestat_get_reports_file_size_and_type() {
    let extra = "\
fn main() {
    let mut i = Instance::new();
    assert_eq!(i.func2(), 0, \"filestat should succeed\");
    assert_eq!(i.mem()[416], 4, \"a regular file has filetype 4\");
    let size = u64::from_le_bytes([
        i.mem()[432], i.mem()[433], i.mem()[434], i.mem()[435],
        i.mem()[436], i.mem()[437], i.mem()[438], i.mem()[439],
    ]);
    assert_eq!(size, 10, \"file size in bytes\");
}
";
    let (bin, dir) = compile("filestat", FILESTAT, extra);
    std::fs::write(dir.join("input.txt"), b"0123456789").expect("write input file");
    let status = Command::new(&bin)
        .current_dir(&dir)
        .status()
        .expect("run generated binary");
    assert!(status.success(), "fd_filestat_get assertions failed");
}

// With a file table, fd_fdstat_get must describe opened files and the preopen
// directory (not just stdio) — Rust's `File::open` calls it right after
// `path_open`, so returning EBADF there would break every real file open.
// Opens "input.txt" (fd 4) and writes fd 4's fdstat at 400 and fd 3's at 424.
const FDSTAT_FILE: &str = r#"
    (module
      (import "wasi_snapshot_preview1" "path_open"
        (func $po (param i32 i32 i32 i32 i32 i64 i64 i32 i32) (result i32)))
      (import "wasi_snapshot_preview1" "fd_fdstat_get"
        (func $fd (param i32 i32) (result i32)))
      (memory 1)
      (data (i32.const 100) "input.txt")
      (func (export "run") (result i32)
        (drop (call $po (i32.const 3) (i32.const 0) (i32.const 100) (i32.const 9)
                        (i32.const 0) (i64.const 2) (i64.const 0) (i32.const 0) (i32.const 300)))
        (drop (call $fd (i32.const 3) (i32.const 424)))
        (call $fd (i32.load (i32.const 300)) (i32.const 400))))
    "#;

#[test]
fn fd_fdstat_get_describes_open_files_and_the_preopen_dir() {
    let extra = "\
fn main() {
    let mut i = Instance::new();
    assert_eq!(i.func2(), 0, \"fdstat on an open file should succeed\");
    assert_eq!(i.mem()[400], 4, \"an opened file has filetype 4 (regular file)\");
    assert_eq!(i.mem()[424], 3, \"the preopen fd 3 has filetype 3 (directory)\");
}
";
    let (bin, dir) = compile("fdstat_file", FDSTAT_FILE, extra);
    std::fs::write(dir.join("input.txt"), b"hi").expect("write input file");
    let status = Command::new(&bin)
        .current_dir(&dir)
        .status()
        .expect("run generated binary");
    assert!(status.success(), "fd_fdstat_get file assertions failed");
}

// path_open must refuse to escape the preopen directory. "../escape" resolves
// above the preopen root, so it returns ENOTCAPABLE (76) without opening.
const ESCAPE: &str = r#"
    (module
      (import "wasi_snapshot_preview1" "path_open"
        (func $po (param i32 i32 i32 i32 i32 i64 i64 i32 i32) (result i32)))
      (memory 1)
      (data (i32.const 100) "../escape")
      (func (export "run") (result i32)
        (call $po (i32.const 3) (i32.const 0) (i32.const 100) (i32.const 9)
                  (i32.const 0) (i64.const 2) (i64.const 0) (i32.const 0) (i32.const 300))))
    "#;

#[test]
fn path_open_rejects_parent_directory_escape() {
    let extra = "\
fn main() {
    let mut i = Instance::new();
    assert_eq!(i.func1(), 76, \"escaping the preopen must return ENOTCAPABLE\");
}
";
    let (bin, dir) = compile("escape", ESCAPE, extra);
    let status = Command::new(&bin)
        .current_dir(&dir)
        .status()
        .expect("run generated binary");
    assert!(status.success(), "escape rejection assertions failed");
}

// fd_pread reads at an explicit offset without moving the file position. Opens
// "input.txt", then preads at offset 5 into an iovec (offset 0 -> buffer 200,
// cap 64); `run` returns the errno and stores the byte count at offset 8.
const PREAD: &str = r#"
    (module
      (import "wasi_snapshot_preview1" "path_open"
        (func $po (param i32 i32 i32 i32 i32 i64 i64 i32 i32) (result i32)))
      (import "wasi_snapshot_preview1" "fd_pread"
        (func $pr (param i32 i32 i32 i64 i32) (result i32)))
      (memory 1)
      (data (i32.const 100) "input.txt")
      (data (i32.const 0) "\c8\00\00\00\40\00\00\00")
      (func (export "run") (result i32)
        (drop (call $po (i32.const 3) (i32.const 0) (i32.const 100) (i32.const 9)
                        (i32.const 0) (i64.const 2) (i64.const 0) (i32.const 0) (i32.const 300)))
        (call $pr (i32.load (i32.const 300)) (i32.const 0) (i32.const 1) (i64.const 5) (i32.const 8))))
    "#;

#[test]
fn fd_pread_reads_at_an_offset() {
    let extra = "\
fn main() {
    let mut i = Instance::new();
    assert_eq!(i.func2(), 0, \"pread should succeed\");
    let n = u32::from_le_bytes([i.mem()[8], i.mem()[9], i.mem()[10], i.mem()[11]]) as usize;
    assert_eq!(&i.mem()[200..200 + n], b\"contents-123\\n\", \"pread reads from offset 5\");
}
";
    let (bin, dir) = compile("pread", PREAD, extra);
    std::fs::write(dir.join("input.txt"), b"file-contents-123\n").expect("write input file");
    let status = Command::new(&bin)
        .current_dir(&dir)
        .status()
        .expect("run generated binary");
    assert!(status.success(), "fd_pread assertions failed");
}

// fd_pwrite writes at an explicit offset without moving the file position
// (`write_at`). Opens an existing writable "pout.txt", pwrites "XYZ" (iovec at
// 0 -> buffer 200, len 3) at offset 4, and stores the byte count at 308.
const PWRITE: &str = r#"
    (module
      (import "wasi_snapshot_preview1" "path_open"
        (func $po (param i32 i32 i32 i32 i32 i64 i64 i32 i32) (result i32)))
      (import "wasi_snapshot_preview1" "fd_pwrite"
        (func $pw (param i32 i32 i32 i64 i32) (result i32)))
      (memory 1)
      (data (i32.const 100) "pout.txt")
      (data (i32.const 0) "\c8\00\00\00\03\00\00\00")
      (data (i32.const 200) "XYZ")
      (func (export "run") (result i32)
        (drop (call $po (i32.const 3) (i32.const 0) (i32.const 100) (i32.const 8)
                        (i32.const 0) (i64.const 64) (i64.const 0) (i32.const 0) (i32.const 300)))
        (call $pw (i32.load (i32.const 300)) (i32.const 0) (i32.const 1) (i64.const 4) (i32.const 308))))
    "#;

#[test]
fn fd_pwrite_writes_at_an_offset() {
    let extra = "\
fn main() {
    let mut i = Instance::new();
    assert_eq!(i.func2(), 0, \"pwrite should succeed\");
    let n = u32::from_le_bytes([i.mem()[308], i.mem()[309], i.mem()[310], i.mem()[311]]);
    assert_eq!(n, 3, \"three bytes written\");
}
";
    let (bin, dir) = compile("pwrite", PWRITE, extra);
    let out_path = dir.join("pout.txt");
    std::fs::write(&out_path, b"AAAAAAAAAA").expect("seed output file");
    let status = Command::new(&bin)
        .current_dir(&dir)
        .status()
        .expect("run generated binary");
    assert!(status.success(), "fd_pwrite assertions failed");
    let written = std::fs::read(&out_path).expect("output file should exist");
    assert_eq!(written, b"AAAAXYZAAA", "pwrite overwrites bytes 4..7");
}

// path_filestat_get stats a path within the preopen without opening it, so it
// needs no file-descriptor table. It writes a 64-byte filestat at offset 400
// for "input.txt"; `run` returns the errno.
const PATH_FILESTAT: &str = r#"
    (module
      (import "wasi_snapshot_preview1" "path_filestat_get"
        (func $pf (param i32 i32 i32 i32 i32) (result i32)))
      (memory 1)
      (data (i32.const 100) "input.txt")
      (func (export "run") (result i32)
        (call $pf (i32.const 3) (i32.const 0) (i32.const 100) (i32.const 9) (i32.const 400))))
    "#;

#[test]
fn path_filestat_get_reports_size_and_type_without_opening() {
    let extra = "\
fn main() {
    let mut i = Instance::new();
    assert_eq!(i.func1(), 0, \"path_filestat_get should succeed\");
    assert_eq!(i.mem()[416], 4, \"a regular file has filetype 4\");
    let size = u64::from_le_bytes([
        i.mem()[432], i.mem()[433], i.mem()[434], i.mem()[435],
        i.mem()[436], i.mem()[437], i.mem()[438], i.mem()[439],
    ]);
    assert_eq!(size, 10, \"file size in bytes\");
}
";
    let (bin, dir) = compile("path_filestat", PATH_FILESTAT, extra);
    std::fs::write(dir.join("input.txt"), b"0123456789").expect("write input file");
    let status = Command::new(&bin)
        .current_dir(&dir)
        .status()
        .expect("run generated binary");
    assert!(status.success(), "path_filestat_get assertions failed");
}

#[test]
fn path_filestat_get_rejects_parent_directory_escape() {
    // Like path_open, a path escaping the preopen via ".." is ENOTCAPABLE (76).
    let wat = r#"
        (module
          (import "wasi_snapshot_preview1" "path_filestat_get"
            (func $pf (param i32 i32 i32 i32 i32) (result i32)))
          (memory 1)
          (data (i32.const 100) "../escape")
          (func (export "run") (result i32)
            (call $pf (i32.const 3) (i32.const 0) (i32.const 100) (i32.const 9) (i32.const 400))))
        "#;
    let extra = "\
fn main() {
    let mut i = Instance::new();
    assert_eq!(i.func1(), 76, \"escaping the preopen must return ENOTCAPABLE\");
}
";
    let (bin, dir) = compile("path_filestat_escape", wat, extra);
    let status = Command::new(&bin)
        .current_dir(&dir)
        .status()
        .expect("run generated binary");
    assert!(
        status.success(),
        "path_filestat_get escape rejection failed"
    );
}

// path_create_directory makes a directory within the preopen without a file
// table. "newdir" (6 bytes) at offset 100; `run` returns the errno.
const CREATE_DIR: &str = r#"
    (module
      (import "wasi_snapshot_preview1" "path_create_directory"
        (func $md (param i32 i32 i32) (result i32)))
      (memory 1)
      (data (i32.const 100) "newdir")
      (func (export "run") (result i32)
        (call $md (i32.const 3) (i32.const 100) (i32.const 6))))
    "#;

#[test]
fn path_create_directory_makes_a_dir() {
    let extra = "\
fn main() {
    let mut i = Instance::new();
    assert_eq!(i.func1(), 0, \"create_directory should succeed\");
}
";
    let (bin, dir) = compile("create_dir", CREATE_DIR, extra);
    let made = dir.join("newdir");
    let _ = std::fs::remove_dir(&made);
    let status = Command::new(&bin)
        .current_dir(&dir)
        .status()
        .expect("run generated binary");
    assert!(status.success(), "create_directory assertions failed");
    assert!(made.is_dir(), "the directory should exist on disk");
    let _ = std::fs::remove_dir(&made);
}

// path_remove_directory removes an (empty) directory within the preopen.
// "rmdir" (5 bytes) at offset 100; `run` returns the errno.
const RM_DIR: &str = r#"
    (module
      (import "wasi_snapshot_preview1" "path_remove_directory"
        (func $rd (param i32 i32 i32) (result i32)))
      (memory 1)
      (data (i32.const 100) "rmdir")
      (func (export "run") (result i32)
        (call $rd (i32.const 3) (i32.const 100) (i32.const 5))))
    "#;

#[test]
fn path_remove_directory_removes_a_dir() {
    let extra = "\
fn main() {
    let mut i = Instance::new();
    assert_eq!(i.func1(), 0, \"remove_directory should succeed\");
}
";
    let (bin, dir) = compile("rm_dir", RM_DIR, extra);
    let target = dir.join("rmdir");
    std::fs::create_dir_all(&target).expect("seed dir");
    let status = Command::new(&bin)
        .current_dir(&dir)
        .status()
        .expect("run generated binary");
    assert!(status.success(), "remove_directory assertions failed");
    assert!(!target.exists(), "the directory should be gone");
}

// path_unlink_file deletes a regular file within the preopen. "victim.txt"
// (10 bytes) at offset 100; `run` returns the errno.
const UNLINK: &str = r#"
    (module
      (import "wasi_snapshot_preview1" "path_unlink_file"
        (func $ul (param i32 i32 i32) (result i32)))
      (memory 1)
      (data (i32.const 100) "victim.txt")
      (func (export "run") (result i32)
        (call $ul (i32.const 3) (i32.const 100) (i32.const 10))))
    "#;

#[test]
fn path_unlink_file_deletes_a_file() {
    let extra = "\
fn main() {
    let mut i = Instance::new();
    assert_eq!(i.func1(), 0, \"unlink_file should succeed\");
}
";
    let (bin, dir) = compile("unlink", UNLINK, extra);
    let victim = dir.join("victim.txt");
    std::fs::write(&victim, b"delete me").expect("seed file");
    let status = Command::new(&bin)
        .current_dir(&dir)
        .status()
        .expect("run generated binary");
    assert!(status.success(), "unlink_file assertions failed");
    assert!(!victim.exists(), "the file should be gone");
}

// path_rename renames a file within the preopen (both dirfds are 3). "old.txt"
// (7 bytes) at 100 and "new.txt" (7 bytes) at 120; `run` returns the errno.
const RENAME: &str = r#"
    (module
      (import "wasi_snapshot_preview1" "path_rename"
        (func $mv (param i32 i32 i32 i32 i32 i32) (result i32)))
      (memory 1)
      (data (i32.const 100) "old.txt")
      (data (i32.const 120) "new.txt")
      (func (export "run") (result i32)
        (call $mv (i32.const 3) (i32.const 100) (i32.const 7)
                  (i32.const 3) (i32.const 120) (i32.const 7))))
    "#;

#[test]
fn path_rename_moves_a_file() {
    let extra = "\
fn main() {
    let mut i = Instance::new();
    assert_eq!(i.func1(), 0, \"rename should succeed\");
}
";
    let (bin, dir) = compile("rename", RENAME, extra);
    let old = dir.join("old.txt");
    let new = dir.join("new.txt");
    let _ = std::fs::remove_file(&new);
    std::fs::write(&old, b"renamed-contents").expect("seed file");
    let status = Command::new(&bin)
        .current_dir(&dir)
        .status()
        .expect("run generated binary");
    assert!(status.success(), "rename assertions failed");
    assert!(!old.exists(), "the old name should be gone");
    assert_eq!(
        std::fs::read(&new).expect("renamed file should exist"),
        b"renamed-contents",
    );
    let _ = std::fs::remove_file(&new);
}

// path_symlink creates a symlink within the preopen. The target "input.txt"
// (9 bytes, link *contents*, not containment-checked) is at 100; the link path
// "link.txt" (8 bytes) at 120; `run` returns the errno.
const SYMLINK: &str = r#"
    (module
      (import "wasi_snapshot_preview1" "path_symlink"
        (func $sl (param i32 i32 i32 i32 i32) (result i32)))
      (memory 1)
      (data (i32.const 100) "input.txt")
      (data (i32.const 120) "link.txt")
      (func (export "run") (result i32)
        (call $sl (i32.const 100) (i32.const 9) (i32.const 3) (i32.const 120) (i32.const 8))))
    "#;

#[test]
fn path_symlink_creates_a_symlink() {
    let extra = "\
fn main() {
    let mut i = Instance::new();
    assert_eq!(i.func1(), 0, \"symlink should succeed\");
}
";
    let (bin, dir) = compile("symlink", SYMLINK, extra);
    let link = dir.join("link.txt");
    let _ = std::fs::remove_file(&link);
    std::fs::write(dir.join("input.txt"), b"target-data").expect("seed target");
    let status = Command::new(&bin)
        .current_dir(&dir)
        .status()
        .expect("run generated binary");
    assert!(status.success(), "symlink assertions failed");
    let meta = std::fs::symlink_metadata(&link).expect("link should exist");
    assert!(
        meta.file_type().is_symlink(),
        "link.txt should be a symlink"
    );
    assert_eq!(
        std::fs::read_link(&link).expect("read_link"),
        std::path::Path::new("input.txt"),
        "the symlink target is stored verbatim",
    );
    assert_eq!(
        std::fs::read(&link).expect("read through symlink"),
        b"target-data",
    );
    let _ = std::fs::remove_file(&link);
}

#[test]
fn path_create_directory_rejects_parent_directory_escape() {
    // Like path_open, a path escaping the preopen via ".." is ENOTCAPABLE (76).
    let wat = r#"
        (module
          (import "wasi_snapshot_preview1" "path_create_directory"
            (func $md (param i32 i32 i32) (result i32)))
          (memory 1)
          (data (i32.const 100) "../escape")
          (func (export "run") (result i32)
            (call $md (i32.const 3) (i32.const 100) (i32.const 9))))
        "#;
    let extra = "\
fn main() {
    let mut i = Instance::new();
    assert_eq!(i.func1(), 76, \"escaping the preopen must return ENOTCAPABLE\");
}
";
    let (bin, dir) = compile("create_dir_escape", wat, extra);
    let status = Command::new(&bin)
        .current_dir(&dir)
        .status()
        .expect("run generated binary");
    assert!(status.success(), "create_directory escape rejection failed");
}

// fd_readdir on the preopen fd 3 enumerates the current directory. The listing
// must contain the synthetic "." and ".." plus a real marker file on disk.
// Buffer at 1000 (len 4096); bufused stored at 400; `run` returns the errno.
// Importing fd_readdir alone still provisions the file table (it forces it).
const READDIR_FD3: &str = r#"
    (module
      (import "wasi_snapshot_preview1" "fd_readdir"
        (func $rd (param i32 i32 i32 i64 i32) (result i32)))
      (memory 1)
      (func (export "run") (result i32)
        (call $rd (i32.const 3) (i32.const 1000) (i32.const 4096) (i64.const 0) (i32.const 400))))
    "#;

#[test]
fn fd_readdir_on_preopen_lists_current_dir() {
    let extra = r#"
fn parse(mem: &[u8], base: usize, bufused: usize) -> Vec<(String, u8)> {
    let mut out = Vec::new();
    let mut off = 0usize;
    while off + 24 <= bufused {
        let hb = base + off;
        let namlen =
            u32::from_le_bytes([mem[hb + 16], mem[hb + 17], mem[hb + 18], mem[hb + 19]]) as usize;
        let dtype = mem[hb + 20];
        if off + 24 + namlen > bufused {
            break;
        }
        off += 24;
        let name = String::from_utf8(mem[base + off..base + off + namlen].to_vec()).unwrap();
        off += namlen;
        out.push((name, dtype));
    }
    out
}

fn main() {
    let mut i = Instance::new();
    assert_eq!(i.func1(), 0, "readdir on fd 3 should succeed");
    let bufused =
        u32::from_le_bytes([i.mem()[400], i.mem()[401], i.mem()[402], i.mem()[403]]) as usize;
    let names: Vec<String> = parse(i.mem(), 1000, bufused)
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    assert!(names.iter().any(|n| n == "."), "listing must contain .");
    assert!(names.iter().any(|n| n == ".."), "listing must contain ..");
    assert!(
        names.iter().any(|n| n == "marker.txt"),
        "listing must contain the marker file, got {names:?}"
    );
}
"#;
    let (bin, dir) = compile("readdir_fd3", READDIR_FD3, extra);
    std::fs::write(dir.join("marker.txt"), b"m").expect("write marker file");
    let status = Command::new(&bin)
        .current_dir(&dir)
        .status()
        .expect("run generated binary");
    assert!(status.success(), "fd_readdir fd 3 assertions failed");
}

// fd_readdir enumerates a directory opened via path_open. A subdir "rd" holds
// "a.txt" (file), "b.txt" (file), and "sub" (dir). path_open("rd") yields fd 4;
// fd_readdir(4) packs dirents into a 512-byte buffer at 1000, storing bufused
// at 400. The listing must be exactly {".","..","a.txt","b.txt","sub"} with the
// right filetypes (directory=3, regular file=4).
const READDIR_SUBDIR: &str = r#"
    (module
      (import "wasi_snapshot_preview1" "path_open"
        (func $po (param i32 i32 i32 i32 i32 i64 i64 i32 i32) (result i32)))
      (import "wasi_snapshot_preview1" "fd_readdir"
        (func $rd (param i32 i32 i32 i64 i32) (result i32)))
      (memory 1)
      (data (i32.const 100) "rd")
      (func (export "run") (result i32)
        (drop (call $po (i32.const 3) (i32.const 0) (i32.const 100) (i32.const 2)
                        (i32.const 0) (i64.const 2) (i64.const 0) (i32.const 0) (i32.const 300)))
        (call $rd (i32.load (i32.const 300)) (i32.const 1000) (i32.const 512) (i64.const 0) (i32.const 400))))
    "#;

#[test]
fn fd_readdir_lists_an_opened_subdirectory_with_filetypes() {
    let extra = r#"
fn parse(mem: &[u8], base: usize, bufused: usize) -> Vec<(String, u8)> {
    let mut out = Vec::new();
    let mut off = 0usize;
    while off + 24 <= bufused {
        let hb = base + off;
        let namlen =
            u32::from_le_bytes([mem[hb + 16], mem[hb + 17], mem[hb + 18], mem[hb + 19]]) as usize;
        let dtype = mem[hb + 20];
        if off + 24 + namlen > bufused {
            break;
        }
        off += 24;
        let name = String::from_utf8(mem[base + off..base + off + namlen].to_vec()).unwrap();
        off += namlen;
        out.push((name, dtype));
    }
    out
}

fn main() {
    let mut i = Instance::new();
    assert_eq!(i.func2(), 0, "readdir on the opened subdir should succeed");
    let bufused =
        u32::from_le_bytes([i.mem()[400], i.mem()[401], i.mem()[402], i.mem()[403]]) as usize;
    let mut got = parse(i.mem(), 1000, bufused);
    got.sort();
    let mut want = vec![
        (".".to_string(), 3u8),
        ("..".to_string(), 3u8),
        ("a.txt".to_string(), 4u8),
        ("b.txt".to_string(), 4u8),
        ("sub".to_string(), 3u8),
    ];
    want.sort();
    assert_eq!(got, want, "readdir listing mismatch");
}
"#;
    let (bin, dir) = compile("readdir_subdir", READDIR_SUBDIR, extra);
    let rd = dir.join("rd");
    let _ = std::fs::remove_dir_all(&rd);
    std::fs::create_dir_all(rd.join("sub")).expect("seed subdir");
    std::fs::write(rd.join("a.txt"), b"a").expect("seed a.txt");
    std::fs::write(rd.join("b.txt"), b"b").expect("seed b.txt");
    let status = Command::new(&bin)
        .current_dir(&dir)
        .status()
        .expect("run generated binary");
    assert!(status.success(), "fd_readdir subdir assertions failed");
    let _ = std::fs::remove_dir_all(&rd);
}

// fd_readdir paginates: with a 40-byte buffer that holds one full record plus a
// truncated next header, the guest resumes from the last complete entry's
// `d_next` cookie until a call comes back un-truncated (bufused < buf_len). The
// accumulated set must still be the full directory. `open` opens "rd" (fd at
// 300); `readdir` takes a cookie and reads at buffer 1000 (len 40), bufused 400.
const READDIR_PAGED: &str = r#"
    (module
      (import "wasi_snapshot_preview1" "path_open"
        (func $po (param i32 i32 i32 i32 i32 i64 i64 i32 i32) (result i32)))
      (import "wasi_snapshot_preview1" "fd_readdir"
        (func $rd (param i32 i32 i32 i64 i32) (result i32)))
      (memory 1)
      (data (i32.const 100) "rd")
      (func (export "open") (result i32)
        (call $po (i32.const 3) (i32.const 0) (i32.const 100) (i32.const 2)
                  (i32.const 0) (i64.const 2) (i64.const 0) (i32.const 0) (i32.const 300)))
      (func (export "readdir") (param i64) (result i32)
        (call $rd (i32.load (i32.const 300)) (i32.const 1000) (i32.const 40) (local.get 0) (i32.const 400))))
    "#;

#[test]
fn fd_readdir_paginates_across_a_small_buffer() {
    let extra = r#"
fn main() {
    const BUF: usize = 1000;
    const BUF_LEN: usize = 40;
    let mut i = Instance::new();
    assert_eq!(i.func2(), 0, "open should succeed");
    let mut cookie: i64 = 0;
    let mut all: Vec<String> = Vec::new();
    loop {
        assert_eq!(i.func3(cookie), 0, "readdir page should succeed");
        let bufused =
            u32::from_le_bytes([i.mem()[400], i.mem()[401], i.mem()[402], i.mem()[403]]) as usize;
        let mut off = 0usize;
        let mut last_next: Option<i64> = None;
        while off + 24 <= bufused {
            let hb = BUF + off;
            let dnext = u64::from_le_bytes([
                i.mem()[hb], i.mem()[hb + 1], i.mem()[hb + 2], i.mem()[hb + 3],
                i.mem()[hb + 4], i.mem()[hb + 5], i.mem()[hb + 6], i.mem()[hb + 7],
            ]) as i64;
            let namlen = u32::from_le_bytes([
                i.mem()[hb + 16], i.mem()[hb + 17], i.mem()[hb + 18], i.mem()[hb + 19],
            ]) as usize;
            if off + 24 + namlen > bufused {
                break;
            }
            off += 24;
            all.push(String::from_utf8(i.mem()[BUF + off..BUF + off + namlen].to_vec()).unwrap());
            off += namlen;
            last_next = Some(dnext);
        }
        if bufused < BUF_LEN {
            break;
        }
        match last_next {
            Some(n) => cookie = n,
            None => panic!("buffer too small to hold even one entry"),
        }
    }
    all.sort();
    let mut want = vec![
        ".".to_string(),
        "..".to_string(),
        "a.txt".to_string(),
        "b.txt".to_string(),
        "sub".to_string(),
    ];
    want.sort();
    assert_eq!(all, want, "paginated readdir lost or duplicated entries");
}
"#;
    let (bin, dir) = compile("readdir_paged", READDIR_PAGED, extra);
    let rd = dir.join("rd");
    let _ = std::fs::remove_dir_all(&rd);
    std::fs::create_dir_all(rd.join("sub")).expect("seed subdir");
    std::fs::write(rd.join("a.txt"), b"a").expect("seed a.txt");
    std::fs::write(rd.join("b.txt"), b"b").expect("seed b.txt");
    let status = Command::new(&bin)
        .current_dir(&dir)
        .status()
        .expect("run generated binary");
    assert!(status.success(), "fd_readdir pagination assertions failed");
    let _ = std::fs::remove_dir_all(&rd);
}

// path_link creates a hard link within the preopen (both dirfds are 3, both
// paths contained). "orig.txt" (8 bytes) at 100 and "hard.txt" (8 bytes) at
// 120; `run` returns the errno. The link must share the original's contents.
const LINK: &str = r#"
    (module
      (import "wasi_snapshot_preview1" "path_link"
        (func $ln (param i32 i32 i32 i32 i32 i32 i32) (result i32)))
      (memory 1)
      (data (i32.const 100) "orig.txt")
      (data (i32.const 120) "hard.txt")
      (func (export "run") (result i32)
        (call $ln (i32.const 3) (i32.const 0) (i32.const 100) (i32.const 8)
                  (i32.const 3) (i32.const 120) (i32.const 8))))
    "#;

#[test]
fn path_link_creates_a_hard_link() {
    let extra = r#"
fn main() {
    let mut i = Instance::new();
    assert_eq!(i.func1(), 0, "path_link should succeed");
}
"#;
    let (bin, dir) = compile("link", LINK, extra);
    let orig = dir.join("orig.txt");
    let hard = dir.join("hard.txt");
    let _ = std::fs::remove_file(&hard);
    std::fs::write(&orig, b"linked-contents").expect("seed original");
    let status = Command::new(&bin)
        .current_dir(&dir)
        .status()
        .expect("run generated binary");
    assert!(status.success(), "path_link assertions failed");
    assert_eq!(
        std::fs::read(&hard).expect("hard link should exist"),
        b"linked-contents",
    );
    use std::os::unix::fs::MetadataExt;
    assert_eq!(
        std::fs::metadata(&orig).expect("orig meta").ino(),
        std::fs::metadata(&hard).expect("hard meta").ino(),
        "a hard link shares the original's inode",
    );
    let _ = std::fs::remove_file(&hard);
}

#[test]
fn fd_readdir_rejects_a_bogus_fd() {
    // A descriptor that was never opened is EBADF (8).
    let wat = r#"
        (module
          (import "wasi_snapshot_preview1" "fd_readdir"
            (func $rd (param i32 i32 i32 i64 i32) (result i32)))
          (memory 1)
          (func (export "run") (result i32)
            (call $rd (i32.const 99) (i32.const 1000) (i32.const 512) (i64.const 0) (i32.const 400))))
        "#;
    let extra = r#"
fn main() {
    let mut i = Instance::new();
    assert_eq!(i.func1(), 8, "readdir on a bogus fd must return EBADF");
}
"#;
    let (bin, dir) = compile("readdir_bogus", wat, extra);
    let status = Command::new(&bin)
        .current_dir(&dir)
        .status()
        .expect("run generated binary");
    assert!(status.success(), "fd_readdir bogus fd assertions failed");
}
