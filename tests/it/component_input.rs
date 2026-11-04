//! A WebAssembly *component* is rejected with a clear error instead of being
//! mis-parsed as a core module.

use std::process::Command;

use wasm2rs::{dump, transpile};

fn component() -> Vec<u8> {
    wat::parse_str("(component (core module (func (export \"f\"))))").unwrap()
}

#[test]
fn transpile_rejects_a_component() {
    let err = transpile(&component()).unwrap_err().to_string();
    assert!(err.contains("component"), "{err}");
}

#[test]
fn dump_rejects_a_component() {
    let err = dump(&component()).map(|_| ()).unwrap_err().to_string();
    assert!(err.contains("component"), "{err}");
}

#[test]
fn a_core_module_still_transpiles() {
    let wasm = wat::parse_str("(module (func (export \"f\")))").unwrap();
    assert!(transpile(&wasm).is_ok());
    assert!(dump(&wasm).is_ok());
}

#[test]
fn the_cli_fails_on_a_component() {
    let dir = std::env::temp_dir().join(format!("wasm2rs_component_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("c.wasm");
    std::fs::write(&input, component()).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_wasm2rs"))
        .arg(&input)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("component"), "{stderr}");
}
