//! WASI shims form guest `ptr + len` (and the iovec walk `iovs + i * 8`) in
//! `u64` through `wasi_span`, so a guest pointer near 0xFFFF_FFFF cannot wrap to
//! low memory on a 32-bit `usize` host. Out-of-range spans trap like any other
//! out-of-bounds guest access.

use std::process::Command;

use crate::common;

/// `fd_write(1, 0, 1, 100)` over an iovec at address 0 whose buffer is
/// `0xFFFF_FFFC..` + 8 bytes: the end (2^32 + 4) is past any 32-bit address.
const IOV_PAST_4G: &str = r#"(module
  (import "wasi_snapshot_preview1" "fd_write"
    (func $fd_write (param i32 i32 i32 i32) (result i32)))
  (memory 1)
  (data (i32.const 0) "\fc\ff\ff\ff\08\00\00\00")
  (func (export "run") (result i32)
    i32.const 1 i32.const 0 i32.const 1 i32.const 100 call $fd_write))"#;

/// `random_get(0, 4)` storing nothing special, then `fd_write`'s `nwritten`
/// pointer at `0xFFFF_FFFE` (a 4-byte store ending at 2^32 + 2).
const NWRITTEN_PAST_4G: &str = r#"(module
  (import "wasi_snapshot_preview1" "fd_write"
    (func $fd_write (param i32 i32 i32 i32) (result i32)))
  (memory 1)
  (data (i32.const 0) "\10\00\00\00\00\00\00\00")
  (func (export "run") (result i32)
    i32.const 1 i32.const 0 i32.const 1 i32.const -2 call $fd_write))"#;

#[test]
fn iovec_buffer_past_4g_traps_on_host() {
    common::expect_trap(
        "wasi_wrap_iov_host",
        IOV_PAST_4G,
        "let mut m = Instance::new(); m.func1();",
    );
}

#[test]
fn nwritten_pointer_past_4g_traps_on_host() {
    common::expect_trap(
        "wasi_wrap_nwritten_host",
        NWRITTEN_PAST_4G,
        "let mut m = Instance::new(); m.func1();",
    );
}

#[test]
fn shims_do_not_add_guest_pointers_in_usize() {
    let wat = r#"(module
      (import "wasi_snapshot_preview1" "fd_write"
        (func (param i32 i32 i32 i32) (result i32)))
      (import "wasi_snapshot_preview1" "fd_read"
        (func (param i32 i32 i32 i32) (result i32)))
      (import "wasi_snapshot_preview1" "args_get"
        (func (param i32 i32) (result i32)))
      (import "wasi_snapshot_preview1" "fd_readdir"
        (func (param i32 i32 i32 i64 i32) (result i32)))
      (import "wasi_snapshot_preview1" "path_open"
        (func (param i32 i32 i32 i32 i32 i64 i64 i32 i32) (result i32)))
      (import "wasi_snapshot_preview1" "poll_oneoff"
        (func (param i32 i32 i32 i32) (result i32)))
      (memory 1))"#;
    let generated = wasm2rs::transpile(&wat::parse_str(wat).unwrap()).unwrap();
    assert!(generated.contains("fn wasi_span("), "{generated}");
    for needle in [
        "+ i * 8",
        "[ptr..ptr + ",
        "[w..w + ",
        "[pb..pb + ",
        "[buf + used",
    ] {
        assert!(
            !generated.contains(needle),
            "usize pointer arithmetic `{needle}` survives:\n{generated}"
        );
    }
}

/// Run the generated code with a real 32-bit `usize` (wasm32 under node's
/// WASI). Skipped when the target or node is not installed.
#[test]
fn spans_past_4g_trap_with_32bit_usize() {
    let target_ok = Command::new("rustc")
        .args(["--print", "target-libdir", "--target", "wasm32-wasip1"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .is_some_and(|d| std::path::Path::new(d.trim()).is_dir());
    let node_ok = Command::new("node").arg("--version").output().is_ok();
    if !target_ok || !node_ok {
        eprintln!("skipped: wasm32-wasip1 target or node unavailable");
        return;
    }
    for (name, wat) in [("iov", IOV_PAST_4G), ("nwritten", NWRITTEN_PAST_4G)] {
        let generated = wasm2rs::transpile(&wat::parse_str(wat).unwrap()).unwrap();
        let program = format!(
            "{generated}\nfn main() {{ let mut m = Instance::new(); println!(\"RESULT {{}}\", m.func1()); }}\n"
        );
        let dir = std::env::temp_dir().join(format!("wasm2rs_wasi_wrap_w32_{name}"));
        std::fs::create_dir_all(&dir).unwrap();
        let (src, wasm, js) = (dir.join("m.rs"), dir.join("m.wasm"), dir.join("run.mjs"));
        std::fs::write(&src, program).unwrap();
        std::fs::write(
            &js,
            "import {WASI} from 'node:wasi'; import fs from 'node:fs';\n\
             const w = new WASI({version: 'preview1', args: [], env: {}});\n\
             const mod = await WebAssembly.compile(fs.readFileSync(process.argv[2]));\n\
             const i = await WebAssembly.instantiate(mod, w.getImportObject());\n\
             try { w.start(i); } catch (e) { console.log('TRAP', e.message); }\n",
        )
        .unwrap();
        // Release: debug builds would panic on a usize overflow and mask a wrap.
        let rc = Command::new("rustc")
            .args(["--edition", "2024", "-O", "--target", "wasm32-wasip1"])
            .arg(&src)
            .arg("-o")
            .arg(&wasm)
            .output()
            .unwrap();
        assert!(
            rc.status.success(),
            "{}",
            String::from_utf8_lossy(&rc.stderr)
        );
        let out = Command::new("node")
            .arg("--no-warnings")
            .arg(&js)
            .arg(&wasm)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            !stdout.contains("RESULT"),
            "{name}: span past 2^32 reached memory on a 32-bit host: {stdout}"
        );
    }
}
