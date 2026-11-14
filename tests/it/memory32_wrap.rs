//! A memory32 access whose effective address `addr + offset` exceeds 32 bits
//! must trap. The helpers used to compute it as `addr as u32 as usize + offset
//! as usize`, which wraps on a 32-bit `usize` host (so `0xFFFF_FFFC + 8` read
//! address 4 instead of trapping). The address is now formed in `u64`.

use std::process::Command;

use crate::common;

/// `i32.load offset=8` at address `0xFFFF_FFFC`: effective address 2^32 + 4.
const WAT: &str = r#"(module
  (memory 1)
  (func (export "run") (result i32) i32.const -4 i32.load offset=8))"#;

#[test]
fn effective_address_overflow_traps_on_host() {
    common::expect_trap(
        "memory32_wrap_host",
        WAT,
        "let mut m = Instance::new(); m.func0();",
    );
}

#[test]
fn helpers_do_not_add_in_usize() {
    let wat = r#"(module (memory 1)
      (func (export "run") (param i32) (result i32)
        local.get 0 i32.load8_u offset=8
        local.get 0 v128.load offset=8 i32x4.extract_lane 0 i32.add))"#;
    let generated = wasm2rs::transpile(&wat::parse_str(wat).unwrap()).unwrap();
    assert!(
        !generated.contains("as u32 as usize + offset as usize"),
        "address computed in usize can wrap on 32-bit hosts:\n{generated}"
    );
}

#[test]
fn atomics_do_not_add_in_usize() {
    let wat = r#"(module (memory 1 1 shared)
      (func (export "run") (param i32) (result i32)
        local.get 0 i32.atomic.load offset=8
        local.get 0 i32.const 1 i32.atomic.rmw.add offset=8 i32.add))"#;
    let generated = wasm2rs::transpile(&wat::parse_str(wat).unwrap()).unwrap();
    assert!(generated.contains("atomic_load("), "{generated}");
    assert!(
        !generated.contains("as u32 as usize +"),
        "atomic address computed in usize can wrap on 32-bit hosts:\n{generated}"
    );
}

/// Run the generated code with a real 32-bit `usize` (wasm32 under node's
/// WASI). Skipped when the target or node is not installed.
#[test]
fn effective_address_overflow_traps_with_32bit_usize() {
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
    let generated = wasm2rs::transpile(&wat::parse_str(WAT).unwrap()).unwrap();
    let program = format!(
        "{generated}\nfn main() {{ let mut m = Instance::new(); println!(\"RESULT {{}}\", m.func0()); }}\n"
    );
    let dir =
        std::env::temp_dir().join(format!("wasm2rs_memory32_wrap_w32_{}", std::process::id()));
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
    // Release mode: the original wrap is silent there (debug builds panic on
    // the overflow, which would mask it).
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
        "access past 2^32 reached memory on a 32-bit host: {stdout}"
    );
}
