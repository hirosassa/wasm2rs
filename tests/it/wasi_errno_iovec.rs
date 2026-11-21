//! Regression tests for the preview1 shims' errno mapping and iovec handling:
//! the OS errno is preferred over the `ErrorKind`, a read of a directory is
//! EISDIR, a failed transfer stores 0 as its byte count, an iovec count above
//! 2^20 is EFAULT before any allocation, and every iovec is validated before any
//! I/O (also for `fd_pread`/`fd_pwrite`). The generated Rust is compiled with
//! `rustc -D warnings` and run against real files (no mocking).

use std::process::Command;

/// Imports 0..=4 are `path_open`, `fd_read`, `fd_write`, `fd_pread`,
/// `fd_pwrite`; the wrappers `func5..=func9` expose them to the test `main`:
/// `open(path, len, oflags, write)` stores the new fd at 300, the others forward
/// their arguments.
const MODULE: &str = r#"
    (module
      (import "wasi_snapshot_preview1" "path_open"
        (func $po (param i32 i32 i32 i32 i32 i64 i64 i32 i32) (result i32)))
      (import "wasi_snapshot_preview1" "fd_read"
        (func $rd (param i32 i32 i32 i32) (result i32)))
      (import "wasi_snapshot_preview1" "fd_write"
        (func $wr (param i32 i32 i32 i32) (result i32)))
      (import "wasi_snapshot_preview1" "fd_pread"
        (func $pr (param i32 i32 i32 i64 i32) (result i32)))
      (import "wasi_snapshot_preview1" "fd_pwrite"
        (func $pw (param i32 i32 i32 i64 i32) (result i32)))
      (memory 1)
      (func (export "open") (param i32 i32 i32 i32) (result i32)
        (call $po (i32.const 3) (i32.const 0) (local.get 0) (local.get 1) (local.get 2)
                  (select (i64.const 64) (i64.const 2) (local.get 3))
                  (i64.const 0) (i32.const 0) (i32.const 300)))
      (func (export "read") (param i32 i32 i32 i32) (result i32)
        (call $rd (local.get 0) (local.get 1) (local.get 2) (local.get 3)))
      (func (export "write") (param i32 i32 i32 i32) (result i32)
        (call $wr (local.get 0) (local.get 1) (local.get 2) (local.get 3)))
      (func (export "pread") (param i32 i32 i32 i64 i32) (result i32)
        (call $pr (local.get 0) (local.get 1) (local.get 2) (local.get 3) (local.get 4)))
      (func (export "pwrite") (param i32 i32 i32 i64 i32) (result i32)
        (call $pw (local.get 0) (local.get 1) (local.get 2) (local.get 3) (local.get 4))))
    "#;

/// Transpile `MODULE`, append `extra`, compile it, and run it in a scratch
/// directory prepared by `setup`.
fn run(test: &str, extra: &str, setup: impl FnOnce(&std::path::Path)) {
    let wasm = wat::parse_str(MODULE).expect("valid wat");
    let generated = wasm2rs::transpile(&wasm).expect("transpile ok");
    let dir = crate::common::scratch_dir(&format!("wasm2rs_wasierr_{test}"));
    let src = dir.join("gen.rs");
    let bin = dir.join(if cfg!(windows) { "gen.exe" } else { "gen" });
    let program = format!("{generated}\n{extra}\n");
    std::fs::write(&src, &program).expect("write generated source");
    let out = Command::new("rustc")
        .current_dir(&dir)
        .arg(&src)
        .args(["--edition", "2024", "-D", "warnings", "-o"])
        .arg(&bin)
        .output()
        .expect("run rustc");
    assert!(
        out.status.success(),
        "generated code failed to compile:\n{program}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&out.stderr),
    );
    setup(&dir);
    let out = Command::new(&bin)
        .current_dir(&dir)
        .output()
        .expect("run generated binary");
    assert!(
        out.status.success(),
        "assertions failed:\n{}",
        String::from_utf8_lossy(&out.stderr),
    );
}

/// Shared prelude of the test `main`s: helpers to place a path, an iovec and
/// read back a u32.
const PRELUDE: &str = r#"
fn put(i: &mut Instance, at: usize, bytes: &[u8]) {
    i.mem_mut()[at..at + bytes.len()].copy_from_slice(bytes);
}
fn u32_at(i: &Instance, at: usize) -> u32 {
    u32::from_le_bytes([i.mem()[at], i.mem()[at + 1], i.mem()[at + 2], i.mem()[at + 3]])
}
fn iov(i: &mut Instance, slot: usize, ptr: u32, len: u32) {
    put(i, slot * 8, &ptr.to_le_bytes());
    put(i, slot * 8 + 4, &len.to_le_bytes());
}
fn open(i: &mut Instance, path: &str, oflags: i32, write: i32) -> (i32, i32) {
    put(i, 2000, path.as_bytes());
    let rc = i.func5(2000, path.len() as i32, oflags, write);
    (rc, u32_at(i, 300) as i32)
}
"#;

#[test]
fn os_errno_is_preferred_and_directories_read_as_eisdir() {
    let extra = format!(
        "{PRELUDE}
fn main() {{
    let mut i = Instance::new();
    // The OS errno wins; the ErrorKind is the fallback.
    for (os, want) in [(1, 63), (2, 44), (3, 71), (6, 60), (9, 8), (13, 2), (17, 20), (21, 31), (28, 51), (32, 64)] {{
        let e = std::io::Error::from_raw_os_error(os);
        assert_eq!(Instance::wasi_errno(&e), want, \"os errno {{os}}\");
    }}
    assert_eq!(Instance::wasi_errno(&std::io::Error::from(std::io::ErrorKind::NotFound)), 44);
    assert_eq!(Instance::wasi_errno(&std::io::Error::from(std::io::ErrorKind::BrokenPipe)), 64);
    assert_eq!(Instance::wasi_errno(&std::io::Error::other(\"x\")), 29);

    // path_open errnos: ENOENT, EEXIST, and EISDIR (writing a directory).
    assert_eq!(open(&mut i, \"missing.txt\", 0, 0).0, 44);
    assert_eq!(open(&mut i, \"data.txt\", 0x1 | 0x4, 1).0, 20);
    assert_eq!(open(&mut i, \"sub\", 0x1, 1).0, 31);

    // Reading a directory is EISDIR, and the count is stored as 0.
    let (rc, dir_fd) = open(&mut i, \"sub\", 0, 0);
    assert_eq!(rc, 0);
    iov(&mut i, 0, 1000, 16);
    put(&mut i, 8, &99u32.to_le_bytes());
    assert_eq!(i.func6(dir_fd, 0, 1, 8), 31);
    assert_eq!(u32_at(&i, 8), 0);
    // So is the preopen root (fd 3), and a positioned read of the directory.
    put(&mut i, 8, &99u32.to_le_bytes());
    assert_eq!(i.func6(3, 0, 1, 8), 31);
    assert_eq!(u32_at(&i, 8), 0);
    put(&mut i, 8, &99u32.to_le_bytes());
    assert_eq!(i.func8(dir_fd, 0, 1, 0, 8), 31);
    assert_eq!(u32_at(&i, 8), 0);
    // A directory was opened read-only, so writing it is EBADF with count 0.
    put(&mut i, 8, &99u32.to_le_bytes());
    assert_eq!(i.func7(dir_fd, 0, 1, 8), 8);
    assert_eq!(u32_at(&i, 8), 0);

    // EOF is a successful 0-byte read (not an error), for read and pread.
    let (rc, empty_fd) = open(&mut i, \"empty.txt\", 0, 0);
    assert_eq!(rc, 0);
    put(&mut i, 8, &99u32.to_le_bytes());
    assert_eq!(i.func6(empty_fd, 0, 1, 8), 0);
    assert_eq!(u32_at(&i, 8), 0);
    put(&mut i, 8, &99u32.to_le_bytes());
    assert_eq!(i.func8(empty_fd, 0, 1, 0, 8), 0);
    assert_eq!(u32_at(&i, 8), 0);

    // A short file reads fully through scattered iovecs.
    let (rc, data_fd) = open(&mut i, \"data.txt\", 0, 0);
    assert_eq!(rc, 0);
    iov(&mut i, 0, 1000, 2);
    iov(&mut i, 1, 1100, 16);
    assert_eq!(i.func6(data_fd, 0, 2, 8), 0);
    assert_eq!(u32_at(&i, 8), 5);
    assert_eq!(&i.mem()[1000..1002], b\"he\");
    assert_eq!(&i.mem()[1100..1103], b\"llo\");
}}
"
    );
    run("errno", &extra, |dir| {
        std::fs::write(dir.join("data.txt"), b"hello").expect("write data");
        std::fs::write(dir.join("empty.txt"), b"").expect("write empty");
        std::fs::create_dir_all(dir.join("sub")).expect("mkdir sub");
    });
}

#[test]
fn iovec_count_cap_and_up_front_validation() {
    let extra = format!(
        "{PRELUDE}
fn main() {{
    let mut i = Instance::new();
    let (rc, fd) = open(&mut i, \"out.txt\", 0x1, 1);
    assert_eq!(rc, 0);

    // 2^20 + 1 iovecs: EFAULT before anything is allocated, count stored as 0.
    let big = (1 << 20) + 1;
    for (name, rc, n_at) in [
        (\"read\", {{ put(&mut i, 8, &99u32.to_le_bytes()); i.func6(fd, 0, big, 8) }}, 8),
        (\"write\", {{ put(&mut i, 8, &99u32.to_le_bytes()); i.func7(fd, 0, big, 8) }}, 8),
        (\"pread\", {{ put(&mut i, 8, &99u32.to_le_bytes()); i.func8(fd, 0, big, 0, 8) }}, 8),
        (\"pwrite\", {{ put(&mut i, 8, &99u32.to_le_bytes()); i.func9(fd, 0, big, 0, 8) }}, 8),
    ] {{
        assert_eq!(rc, 21, \"{{name}}\");
        assert_eq!(u32_at(&i, n_at), 0, \"{{name}} count\");
    }}
    // An iovs_len of -1 (u32::MAX) is refused too.
    assert_eq!(i.func7(fd, 0, -1, 8), 21);

    // Both iovecs hold in-bounds data, but the second is out of bounds: the
    // call traps before any byte is written to the file.
    put(&mut i, 1000, b\"abcd\");
    iov(&mut i, 0, 1000, 4);
    iov(&mut i, 1, 65530, 100);
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| i.func9(fd, 0, 2, 0, 8)));
    assert!(r.is_err(), \"pwrite should trap on the out-of-bounds iovec\");
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| i.func7(fd, 0, 2, 8)));
    assert!(r.is_err(), \"write should trap on the out-of-bounds iovec\");
    assert_eq!(std::fs::read(\"out.txt\").expect(\"read out\"), b\"\");

    // pread validates every iovec up front too, instead of reading then trapping.
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| i.func8(fd, 0, 2, 0, 8)));
    assert!(r.is_err(), \"pread should trap on the out-of-bounds iovec\");

    // With in-bounds iovecs a positioned write and a gathered write both land.
    iov(&mut i, 1, 1004, 2);
    put(&mut i, 1004, b\"ef\");
    assert_eq!(i.func9(fd, 0, 2, 0, 8), 0);
    assert_eq!(u32_at(&i, 8), 6);
    assert_eq!(std::fs::read(\"out.txt\").expect(\"read out\"), b\"abcdef\");
}}
"
    );
    run("iovec", &extra, |_| {});
}
