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
        .arg("2024")
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

// `with_preopen_root` re-points the single preopen away from the process CWD.
// `fd_prestat_dir_name` must then advertise the configured name (its byte
// length in the prestat), so wasi-libc maps guest paths against that root.
#[test]
fn with_preopen_root_advertises_the_configured_name() {
    let extra = "\
fn main() {
    let mut i = Instance::new().with_preopen_root(\"root_sub\");
    assert_eq!(i.func2(), 8, \"probing fd 4 must report EBADF\");
    assert_eq!(i.mem()[0], 0, \"fd 3 should be a preopen directory (tag 0)\");
    let name_len = u32::from_le_bytes([i.mem()[4], i.mem()[5], i.mem()[6], i.mem()[7]]);
    assert_eq!(name_len, 8, \"the preopen name \\\"root_sub\\\" is eight bytes\");
    assert_eq!(&i.mem()[16..24], b\"root_sub\", \"the preopen dir name is the configured root\");
}
";
    let (bin, dir) = compile("preopen_name", PRESTAT, extra);
    let status = Command::new(&bin)
        .current_dir(&dir)
        .status()
        .expect("run generated binary");
    assert!(status.success(), "custom preopen name assertions failed");
}

// With a custom preopen root, `path_open` resolves the guest path *within that
// root*, not the process CWD. The file lives at `<cwd>/root_sub/input.txt`; the
// child runs from `<cwd>`, so only a root redirect finds it (the default "."
// preopen would look for `<cwd>/input.txt`, which does not exist).
#[test]
fn with_preopen_root_redirects_path_open() {
    let extra = "\
fn main() {
    let mut i = Instance::new().with_preopen_root(\"root_sub\");
    let errno = i.func2();
    assert_eq!(errno, 0, \"read through the custom preopen root should succeed\");
    let n = u32::from_le_bytes([i.mem()[8], i.mem()[9], i.mem()[10], i.mem()[11]]) as usize;
    assert_eq!(&i.mem()[200..200 + n], b\"under-the-root\\n\");
}
";
    let (bin, dir) = compile("preopen_open", OPEN_READ, extra);
    let root = dir.join("root_sub");
    std::fs::create_dir_all(&root).expect("create preopen root");
    std::fs::write(root.join("input.txt"), b"under-the-root\n").expect("write input file");
    let _ = std::fs::remove_file(dir.join("input.txt"));
    let status = Command::new(&bin)
        .current_dir(&dir)
        .status()
        .expect("run generated binary");
    assert!(
        status.success(),
        "custom preopen path_open assertions failed"
    );
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

// ---------------------------------------------------------------------------
// fd_tell, fd_sync/fd_datasync, fd_advise, fd_allocate, fd_filestat_set_size,
// fd_filestat_set_times, path_filestat_set_times, path_readlink,
// fd_fdstat_set_flags/rights and fd_renumber.
//
// Every module opens "f.txt" (fd 4) through `path_open` with the descriptor
// stored at 300; the exported-in-order helper functions then exercise one WASI
// call each and return its errno. The generated `main` calls them by index
// (`func{N}`, N = number of imports + position).
// ---------------------------------------------------------------------------

/// `path_open("f.txt")` for read+write, the new fd stored at 300.
const OPEN_F: &str = "(drop (call $po (i32.const 3) (i32.const 0) (i32.const 100) (i32.const 5)
                        (i32.const 0) (i64.const 64) (i64.const 0) (i32.const 0) (i32.const 300)))";
const FD_F: &str = "(i32.load (i32.const 300))";
const PATH_OPEN_IMPORT: &str = r#"(import "wasi_snapshot_preview1" "path_open"
        (func $po (param i32 i32 i32 i32 i32 i64 i64 i32 i32) (result i32)))"#;

/// Compile `wat`, let `seed` prepare the temp directory, then run the
/// generated `main` there and require it to succeed.
fn exec(
    test: &str,
    wat: &str,
    extra: &str,
    seed: impl FnOnce(&std::path::Path),
) -> std::path::PathBuf {
    let (bin, dir) = compile(test, wat, extra);
    seed(&dir);
    let status = Command::new(&bin)
        .current_dir(&dir)
        .status()
        .expect("run generated binary");
    assert!(status.success(), "{test}: generated main assertions failed");
    dir
}

fn fs_module(imports: &str, funcs: &str) -> String {
    format!(
        r#"(module
      {PATH_OPEN_IMPORT}
      {imports}
      (memory 1)
      (data (i32.const 100) "f.txt")
      (data (i32.const 110) "g.txt")
      (data (i32.const 120) "link")
      (data (i32.const 140) "../x")
      {funcs})"#
    )
}

#[test]
fn fd_tell_reports_the_current_offset() {
    let wat = fs_module(
        r#"(import "wasi_snapshot_preview1" "fd_seek"
             (func $seek (param i32 i64 i32 i32) (result i32)))
           (import "wasi_snapshot_preview1" "fd_tell"
             (func $tell (param i32 i32) (result i32)))"#,
        &format!(
            r#"(func (export "run") (result i32)
                 {OPEN_F}
                 (drop (call $seek {FD_F} (i64.const 5) (i32.const 0) (i32.const 308)))
                 (call $tell {FD_F} (i32.const 316)))
               (func (export "stdio") (result i32)
                 (call $tell (i32.const 1) (i32.const 316)))"#
        ),
    );
    let extra = r#"
fn main() {
    let mut i = Instance::new();
    assert_eq!(i.func3(), 0, "tell on an open file succeeds");
    let off = u64::from_le_bytes(i.mem()[316..324].try_into().unwrap());
    assert_eq!(off, 5, "tell reports the offset set by seek");
    assert_eq!(i.func4(), 70, "stdio is not seekable (ESPIPE)");
}
"#;
    exec("fd_tell", &wat, extra, |d| {
        std::fs::write(d.join("f.txt"), b"hello, world!").expect("seed");
    });
}

#[test]
fn fd_sync_and_datasync_flush_files_and_accept_stdio() {
    let wat = fs_module(
        r#"(import "wasi_snapshot_preview1" "fd_sync" (func $sync (param i32) (result i32)))
           (import "wasi_snapshot_preview1" "fd_datasync" (func $dsync (param i32) (result i32)))"#,
        &format!(
            r#"(func (export "file") (result i32)
                 {OPEN_F}
                 (i32.or (call $sync {FD_F}) (call $dsync {FD_F})))
               (func (export "stdio") (result i32)
                 (i32.or (call $sync (i32.const 1)) (call $dsync (i32.const 2))))
               (func (export "bad") (result i32) (call $sync (i32.const 99)))"#
        ),
    );
    let extra = r#"
fn main() {
    let mut i = Instance::new();
    assert_eq!(i.func3(), 0, "sync/datasync on a file succeed");
    assert_eq!(i.func4(), 0, "stdio sync is a no-op success");
    assert_eq!(i.func5(), 8, "unknown fd is EBADF");
}
"#;
    exec("fd_sync", &wat, extra, |d| {
        std::fs::write(d.join("f.txt"), b"data").expect("seed");
    });
}

#[test]
fn fd_advise_validates_fd_and_advice() {
    let wat = fs_module(
        r#"(import "wasi_snapshot_preview1" "fd_advise"
             (func $adv (param i32 i64 i64 i32) (result i32)))"#,
        &format!(
            r#"(func (export "ok") (result i32)
                 {OPEN_F}
                 (call $adv {FD_F} (i64.const 0) (i64.const 4) (i32.const 0)))
               (func (export "bad_advice") (result i32)
                 (call $adv (i32.const 4) (i64.const 0) (i64.const 4) (i32.const 9)))
               (func (export "bad_fd") (result i32)
                 (call $adv (i32.const 99) (i64.const 0) (i64.const 4) (i32.const 0)))"#
        ),
    );
    let extra = r#"
fn main() {
    let mut i = Instance::new();
    assert_eq!(i.func2(), 0, "advice on an open file is accepted");
    assert_eq!(i.func3(), 28, "advice > 5 is EINVAL");
    assert_eq!(i.func4(), 8, "unknown fd is EBADF");
}
"#;
    exec("fd_advise", &wat, extra, |d| {
        std::fs::write(d.join("f.txt"), b"data").expect("seed");
    });
}

#[test]
fn fd_allocate_grows_but_never_shrinks() {
    let wat = fs_module(
        r#"(import "wasi_snapshot_preview1" "fd_allocate"
             (func $alloc (param i32 i64 i64) (result i32)))"#,
        &format!(
            r#"(func (export "grow") (result i32)
                 {OPEN_F}
                 (call $alloc {FD_F} (i64.const 4) (i64.const 6)))
               (func (export "no_shrink") (result i32)
                 (call $alloc {FD_F} (i64.const 0) (i64.const 1)))
               (func (export "stdio") (result i32)
                 (call $alloc (i32.const 1) (i64.const 0) (i64.const 1)))"#
        ),
    );
    let extra = r#"
fn main() {
    let mut i = Instance::new();
    assert_eq!(i.func2(), 0);
    assert_eq!(i.func3(), 0, "a range inside the file leaves its size alone");
    assert_eq!(i.func4(), 70, "stdio is ESPIPE");
}
"#;
    let dir = exec("fd_allocate", &wat, extra, |d| {
        std::fs::write(d.join("f.txt"), b"abc").expect("seed");
    });
    assert_eq!(std::fs::metadata(dir.join("f.txt")).unwrap().len(), 10);
}

#[test]
fn fd_filestat_set_size_truncates_and_extends() {
    let wat = fs_module(
        r#"(import "wasi_snapshot_preview1" "fd_filestat_set_size"
             (func $sz (param i32 i64) (result i32)))"#,
        &format!(
            r#"(func (export "shrink") (result i32)
                 {OPEN_F}
                 (call $sz {FD_F} (i64.const 2)))
               (func (export "grow") (result i32)
                 (call $sz {FD_F} (i64.const 8)))
               (func (export "stdio") (result i32)
                 (call $sz (i32.const 1) (i64.const 0)))"#
        ),
    );
    let extra = r#"
fn main() {
    let mut i = Instance::new();
    assert_eq!(i.func2(), 0);
    assert_eq!(i.func3(), 0);
    assert_eq!(i.func4(), 70, "stdio cannot be resized");
}
"#;
    let dir = exec("fd_set_size", &wat, extra, |d| {
        std::fs::write(d.join("f.txt"), b"abcdef").expect("seed");
    });
    assert_eq!(
        std::fs::read(dir.join("f.txt")).unwrap(),
        b"ab\0\0\0\0\0\0",
        "truncated to 2 then zero-extended to 8",
    );
}

#[test]
fn fd_filestat_set_times_sets_explicit_times() {
    let wat = fs_module(
        r#"(import "wasi_snapshot_preview1" "fd_filestat_set_times"
             (func $st (param i32 i64 i64 i32) (result i32)))"#,
        &format!(
            r#"(func (export "set") (result i32)
                 {OPEN_F}
                 (call $st {FD_F} (i64.const 1000000000000) (i64.const 1700000000000000000)
                           (i32.const 5)))
               (func (export "conflict") (result i32)
                 (call $st {FD_F} (i64.const 0) (i64.const 0) (i32.const 3)))
               (func (export "stdio") (result i32)
                 (call $st (i32.const 1) (i64.const 0) (i64.const 0) (i32.const 1)))"#
        ),
    );
    let extra = r#"
fn main() {
    let mut i = Instance::new();
    assert_eq!(i.func2(), 0);
    assert_eq!(i.func3(), 28, "ATIM and ATIM_NOW together are EINVAL");
    assert_eq!(i.func4(), 70, "stdio has no settable times");
}
"#;
    let dir = exec("fd_set_times", &wat, extra, |d| {
        std::fs::write(d.join("f.txt"), b"x").expect("seed");
    });
    let meta = std::fs::metadata(dir.join("f.txt")).unwrap();
    let at = |secs| std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs);
    assert_eq!(meta.accessed().unwrap(), at(1000));
    assert_eq!(meta.modified().unwrap(), at(1_700_000_000));
}

#[test]
fn path_filestat_set_times_sets_times_and_guards_paths() {
    let wat = fs_module(
        r#"(import "wasi_snapshot_preview1" "path_filestat_set_times"
             (func $pst (param i32 i32 i32 i32 i64 i64 i32) (result i32)))"#,
        r#"(func (export "set") (result i32)
             (call $pst (i32.const 3) (i32.const 1) (i32.const 100) (i32.const 5)
                        (i64.const 1000000000000) (i64.const 1700000000000000000) (i32.const 5)))
           (func (export "nofollow_symlink") (result i32)
             (call $pst (i32.const 3) (i32.const 0) (i32.const 120) (i32.const 4)
                        (i64.const 0) (i64.const 0) (i32.const 5)))
           (func (export "escape") (result i32)
             (call $pst (i32.const 3) (i32.const 1) (i32.const 140) (i32.const 4)
                        (i64.const 0) (i64.const 0) (i32.const 5)))
           (func (export "missing") (result i32)
             (call $pst (i32.const 3) (i32.const 1) (i32.const 110) (i32.const 5)
                        (i64.const 0) (i64.const 0) (i32.const 5)))"#,
    );
    let extra = r#"
fn main() {
    let mut i = Instance::new();
    assert_eq!(i.func2(), 0);
    assert_eq!(i.func3(), 58, "NOFOLLOW on a symlink cannot be honoured (ENOTSUP)");
    assert_eq!(i.func4(), 76, "escaping the preopen is ENOTCAPABLE");
    assert_eq!(i.func5(), 44, "a missing path is ENOENT");
}
"#;
    let dir = exec("path_set_times", &wat, extra, |d| {
        std::fs::write(d.join("f.txt"), b"x").expect("seed");
        let _ = std::fs::remove_file(d.join("g.txt"));
        let _ = std::fs::remove_file(d.join("link"));
        std::os::unix::fs::symlink("f.txt", d.join("link")).expect("symlink");
    });
    let meta = std::fs::metadata(dir.join("f.txt")).unwrap();
    let at = |secs| std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs);
    assert_eq!(meta.accessed().unwrap(), at(1000));
    assert_eq!(meta.modified().unwrap(), at(1_700_000_000));
}

#[test]
fn path_readlink_returns_the_target_and_truncates() {
    let wat = fs_module(
        r#"(import "wasi_snapshot_preview1" "path_readlink"
             (func $rl (param i32 i32 i32 i32 i32 i32) (result i32)))"#,
        r#"(func (export "full") (result i32)
             (call $rl (i32.const 3) (i32.const 120) (i32.const 4)
                       (i32.const 400) (i32.const 64) (i32.const 500)))
           (func (export "short") (result i32)
             (call $rl (i32.const 3) (i32.const 120) (i32.const 4)
                       (i32.const 600) (i32.const 3) (i32.const 700)))
           (func (export "not_a_link") (result i32)
             (call $rl (i32.const 3) (i32.const 100) (i32.const 5)
                       (i32.const 400) (i32.const 64) (i32.const 500)))
           (func (export "escape") (result i32)
             (call $rl (i32.const 3) (i32.const 140) (i32.const 4)
                       (i32.const 400) (i32.const 64) (i32.const 500)))"#,
    );
    let extra = r#"
fn main() {
    let mut i = Instance::new();
    assert_eq!(i.func2(), 0);
    let used = u32::from_le_bytes(i.mem()[500..504].try_into().unwrap());
    assert_eq!(used, 5);
    assert_eq!(&i.mem()[400..405], b"f.txt");
    assert_eq!(i.func3(), 0);
    let used = u32::from_le_bytes(i.mem()[700..704].try_into().unwrap());
    assert_eq!(used, 3, "the result is truncated to the buffer");
    assert_eq!(&i.mem()[600..603], b"f.t");
    assert_eq!(i.func4(), 28, "a regular file is not a link (EINVAL)");
    assert_eq!(i.func5(), 76, "escaping the preopen is ENOTCAPABLE");
}
"#;
    exec("path_readlink", &wat, extra, |d| {
        std::fs::write(d.join("f.txt"), b"x").expect("seed");
        let _ = std::fs::remove_file(d.join("link"));
        std::os::unix::fs::symlink("f.txt", d.join("link")).expect("symlink");
    });
}

#[test]
fn fd_fdstat_set_flags_and_rights() {
    let wat = fs_module(
        r#"(import "wasi_snapshot_preview1" "fd_fdstat_set_flags"
             (func $flags (param i32 i32) (result i32)))
           (import "wasi_snapshot_preview1" "fd_fdstat_set_rights"
             (func $rights (param i32 i64 i64) (result i32)))"#,
        &format!(
            r#"(func (export "clear_flags") (result i32)
                 {OPEN_F}
                 (call $flags {FD_F} (i32.const 0)))
               (func (export "append") (result i32)
                 (call $flags {FD_F} (i32.const 1)))
               (func (export "bad_fd") (result i32)
                 (call $flags (i32.const 99) (i32.const 0)))
               (func (export "rights") (result i32)
                 (call $rights {FD_F} (i64.const 0) (i64.const 0)))
               (func (export "rights_bad_fd") (result i32)
                 (call $rights (i32.const 99) (i64.const 0) (i64.const 0)))"#
        ),
    );
    let extra = r#"
fn main() {
    let mut i = Instance::new();
    assert_eq!(i.func3(), 0, "clearing flags is a no-op success");
    assert_eq!(i.func4(), 58, "changing flags cannot be honoured (ENOTSUP)");
    assert_eq!(i.func5(), 8, "unknown fd is EBADF");
    assert_eq!(i.func6(), 0, "rights are not tracked, so narrowing succeeds");
    assert_eq!(i.func7(), 8, "unknown fd is EBADF");
}
"#;
    exec("fd_set_flags", &wat, extra, |d| {
        std::fs::write(d.join("f.txt"), b"x").expect("seed");
    });
}

#[test]
fn fd_renumber_moves_a_descriptor_over_another() {
    let wat = fs_module(
        r#"(import "wasi_snapshot_preview1" "fd_renumber" (func $ren (param i32 i32) (result i32)))
           (import "wasi_snapshot_preview1" "fd_fdstat_get" (func $fdstat (param i32 i32) (result i32)))
           (import "wasi_snapshot_preview1" "fd_filestat_get" (func $fstat (param i32 i32) (result i32)))"#,
        &format!(
            r#"(func (export "renumber") (result i32)
                 {OPEN_F}
                 (drop (call $po (i32.const 3) (i32.const 0) (i32.const 110) (i32.const 5)
                        (i32.const 0) (i64.const 64) (i64.const 0) (i32.const 0) (i32.const 304)))
                 (call $ren (i32.load (i32.const 300)) (i32.load (i32.const 304))))
               (func (export "old_fd_closed") (result i32)
                 (call $fdstat (i32.load (i32.const 300)) (i32.const 400)))
               (func (export "target_is_f") (result i32)
                 (call $fstat (i32.load (i32.const 304)) (i32.const 448)))
               (func (export "stdio") (result i32) (call $ren (i32.const 1) (i32.const 2)))
               (func (export "unknown") (result i32) (call $ren (i32.const 99) (i32.const 100)))"#
        ),
    );
    let extra = r#"
fn main() {
    let mut i = Instance::new();
    assert_eq!(i.func4(), 0, "renumber of two open files succeeds");
    assert_eq!(i.func5(), 8, "the source descriptor is closed afterwards");
    assert_eq!(i.func6(), 0);
    let size = u64::from_le_bytes(i.mem()[480..488].try_into().unwrap());
    assert_eq!(size, 4, "the target descriptor now refers to f.txt (4 bytes)");
    assert_eq!(i.func7(), 58, "stdio descriptors cannot be renumbered (ENOTSUP)");
    assert_eq!(i.func8(), 8, "unknown descriptors are EBADF");
}
"#;
    exec("fd_renumber", &wat, extra, |d| {
        std::fs::write(d.join("f.txt"), b"AAAA").expect("seed f");
        std::fs::write(d.join("g.txt"), b"BB").expect("seed g");
    });
}

// A guest-created symlink must not carry later path operations out of the
// preopen. `esc -> ../wasm2rs_symlink_outside` is a *relative* target (so
// creating it is legal), but resolving through it lands outside the preopen:
// reading, creating, or truncating there must be ENOTCAPABLE, as must opening
// a dangling symlink whose target is outside (O_CREAT would create it).
// An absolute symlink target is refused up front.
#[test]
fn guest_created_symlink_cannot_escape_the_preopen() {
    let wat = fs_module(
        r#"(import "wasi_snapshot_preview1" "path_symlink"
             (func $sl (param i32 i32 i32 i32 i32) (result i32)))
           (data (i32.const 200) "esc")
           (data (i32.const 210) "../wasm2rs_symlink_outside")
           (data (i32.const 240) "esc/secret.txt")
           (data (i32.const 260) "esc/new.txt")
           (data (i32.const 280) "dang")
           (data (i32.const 290) "../wasm2rs_symlink_outside/new2")
           (data (i32.const 330) "/etc")
           (data (i32.const 340) "abs")"#,
        r#"(func (export "mk_esc") (result i32)
             (call $sl (i32.const 210) (i32.const 26) (i32.const 3) (i32.const 200) (i32.const 3)))
           (func (export "read_through") (result i32)
             (call $po (i32.const 3) (i32.const 0) (i32.const 240) (i32.const 14)
                       (i32.const 0) (i64.const 2) (i64.const 0) (i32.const 0) (i32.const 400)))
           (func (export "create_through") (result i32)
             (call $po (i32.const 3) (i32.const 0) (i32.const 260) (i32.const 11)
                       (i32.const 1) (i64.const 64) (i64.const 0) (i32.const 0) (i32.const 400)))
           (func (export "mk_dangling") (result i32)
             (call $sl (i32.const 290) (i32.const 31) (i32.const 3) (i32.const 280) (i32.const 4)))
           (func (export "create_dangling") (result i32)
             (call $po (i32.const 3) (i32.const 0) (i32.const 280) (i32.const 4)
                       (i32.const 1) (i64.const 64) (i64.const 0) (i32.const 0) (i32.const 400)))
           (func (export "mk_abs") (result i32)
             (call $sl (i32.const 330) (i32.const 4) (i32.const 3) (i32.const 340) (i32.const 3)))"#,
    );
    let extra = r#"
fn main() {
    let mut i = Instance::new();
    assert_eq!(i.func2(), 0, "a relative symlink target may be created");
    assert_eq!(i.func3(), 76, "reading through an escaping symlink must be ENOTCAPABLE");
    assert_eq!(i.func4(), 76, "creating through an escaping symlink must be ENOTCAPABLE");
    assert_eq!(i.func5(), 0);
    assert_eq!(i.func6(), 76, "creating via a dangling escaping symlink must be ENOTCAPABLE");
    assert_eq!(i.func7(), 76, "an absolute symlink target must be ENOTCAPABLE");
}
"#;
    let outside = std::env::temp_dir().join("wasm2rs_symlink_outside");
    std::fs::create_dir_all(&outside).expect("outside dir");
    std::fs::write(outside.join("secret.txt"), b"secret").expect("seed secret");
    let _ = std::fs::remove_file(outside.join("new.txt"));
    let _ = std::fs::remove_file(outside.join("new2"));
    let dir = exec("symlink_escape", &wat, extra, |d| {
        for n in ["esc", "dang", "abs"] {
            let _ = std::fs::remove_file(d.join(n));
        }
    });
    assert!(!outside.join("new.txt").exists(), "nothing created outside");
    assert!(!outside.join("new2").exists(), "nothing created outside");
    assert!(std::fs::symlink_metadata(dir.join("abs")).is_err());
}

// A directory fd keeps only its path, so swapping that path for a symlink after
// the open must not let fd_readdir list outside the preopen: open "d", remove
// it, put `d -> ..` in its place, then readdir the still-open fd (ENOTCAPABLE).
#[test]
fn fd_readdir_on_a_swapped_directory_fd_cannot_escape() {
    let wat = fs_module(
        r#"(import "wasi_snapshot_preview1" "path_create_directory"
             (func $mk (param i32 i32 i32) (result i32)))
           (import "wasi_snapshot_preview1" "path_remove_directory"
             (func $rm (param i32 i32 i32) (result i32)))
           (import "wasi_snapshot_preview1" "path_symlink"
             (func $sl (param i32 i32 i32 i32 i32) (result i32)))
           (import "wasi_snapshot_preview1" "fd_readdir"
             (func $rd (param i32 i32 i32 i64 i32) (result i32)))
           (data (i32.const 200) "d")
           (data (i32.const 210) "..")"#,
        r#"(func (export "setup") (result i32)
             (drop (call $mk (i32.const 3) (i32.const 200) (i32.const 1)))
             (call $po (i32.const 3) (i32.const 0) (i32.const 200) (i32.const 1)
                       (i32.const 0) (i64.const 2) (i64.const 0) (i32.const 0) (i32.const 400)))
           (func (export "swap") (result i32)
             (drop (call $rm (i32.const 3) (i32.const 200) (i32.const 1)))
             (call $sl (i32.const 210) (i32.const 2) (i32.const 3) (i32.const 200) (i32.const 1)))
           (func (export "list") (result i32)
             (call $rd (i32.const 4) (i32.const 500) (i32.const 256) (i64.const 0) (i32.const 404)))"#,
    );
    let extra = r#"
fn main() {
    let mut i = Instance::new();
    assert_eq!(i.func5(), 0);
    assert_eq!(i.func6(), 0);
    assert_eq!(i.func7(), 76, "readdir through a swapped-in symlink must be ENOTCAPABLE");
}
"#;
    exec("readdir_swap", &wat, extra, |d| {
        let _ = std::fs::remove_dir(d.join("d"));
        let _ = std::fs::remove_file(d.join("d"));
    });
}

// path_open hands out the lowest free descriptor (POSIX), and a descriptor
// freed by fd_close or by being the source of an fd_renumber is reused.
// func3 = open "input.txt" -> its fd, func4 = close(fd), func5 = renumber(from, to).
const FD_REUSE: &str = r#"
    (module
      (import "wasi_snapshot_preview1" "path_open"
        (func $po (param i32 i32 i32 i32 i32 i64 i64 i32 i32) (result i32)))
      (import "wasi_snapshot_preview1" "fd_close"
        (func $cl (param i32) (result i32)))
      (import "wasi_snapshot_preview1" "fd_renumber"
        (func $rn (param i32 i32) (result i32)))
      (memory 1)
      (data (i32.const 100) "input.txt")
      (func (export "open") (result i32)
        (drop (call $po (i32.const 3) (i32.const 0) (i32.const 100) (i32.const 9)
                        (i32.const 0) (i64.const 2) (i64.const 0) (i32.const 0) (i32.const 300)))
        (i32.load (i32.const 300)))
      (func (export "close") (param i32) (result i32) (call $cl (local.get 0)))
      (func (export "renumber") (param i32 i32) (result i32)
        (call $rn (local.get 0) (local.get 1))))
    "#;

#[test]
fn path_open_reuses_the_lowest_freed_descriptor() {
    let extra = "\
fn main() {
    let mut i = Instance::new();
    assert_eq!((i.func3(), i.func3(), i.func3()), (4, 5, 6));
    assert_eq!(i.func4(5), 0);
    assert_eq!(i.func4(4), 0);
    assert_eq!(i.func4(4), 0, \"closing twice is a no-op, not a second free slot\");
    assert_eq!(i.func3(), 4, \"the lowest freed descriptor comes back first\");
    assert_eq!(i.func3(), 5);
    assert_eq!(i.func3(), 7, \"no gap left: a fresh descriptor\");
    assert_eq!(i.func5(7, 4), 0);
    assert_eq!(i.func3(), 7, \"the renumbered source slot is free again\");
    assert_eq!(i.func4(6), 0);
    assert_eq!(i.func4(40), 0, \"closing an unopened descriptor frees nothing\");
    assert_eq!(i.func3(), 6);
}
";
    let (bin, dir) = compile("fd_reuse", FD_REUSE, extra);
    std::fs::write(dir.join("input.txt"), b"x").expect("write input file");
    let status = Command::new(&bin)
        .current_dir(&dir)
        .status()
        .expect("run generated binary");
    assert!(status.success(), "descriptor reuse assertions failed");
}
