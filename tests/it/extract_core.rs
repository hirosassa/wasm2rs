//! `extract_core_modules` pulls the core modules out of a component as plain
//! module binaries; `--extract-core` does the same from the command line.

use std::process::Command;

use crate::common;
use wasm2rs::{dump, extract_core_modules, transpile};

const COMPONENT: &str = r#"(component
  (core module (func (export "a") (result i32) i32.const 1))
  (component
    (core module (func (export "b") (result i32) i32.const 2)))
  (core module (func (export "c") (result i32) i32.const 3)))"#;

#[test]
fn modules_come_out_in_order_including_nested_components() {
    let wasm = wat::parse_str(COMPONENT).unwrap();
    let modules = extract_core_modules(&wasm).unwrap();
    assert_eq!(modules.len(), 3);
    for (module, export) in modules.iter().zip(["a", "b", "c"]) {
        let summary = dump(module).unwrap().to_string();
        assert!(summary.contains(export), "{export}: {summary}");
    }
}

#[test]
fn a_core_module_comes_back_unchanged() {
    let wasm = wat::parse_str("(module (func (export \"f\")))").unwrap();
    assert_eq!(extract_core_modules(&wasm).unwrap(), vec![wasm]);
}

#[test]
fn a_component_without_modules_gives_none() {
    let wasm = wat::parse_str("(component)").unwrap();
    assert!(extract_core_modules(&wasm).unwrap().is_empty());
}

#[test]
fn malformed_input_is_an_error() {
    assert!(extract_core_modules(b"not wasm").is_err());
}

#[test]
fn a_truncated_core_module_is_an_error() {
    let mut wasm = wat::parse_str("(module (func (export \"f\")))").unwrap();
    wasm.truncate(wasm.len() - 1);
    assert!(extract_core_modules(&wasm).is_err());
}

#[test]
fn the_cli_rejects_a_missing_directory_and_extra_arguments() {
    let dir = std::env::temp_dir().join(format!("wasm2rs_extract_args_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("c.wasm");
    std::fs::write(&input, wat::parse_str(COMPONENT).unwrap()).unwrap();
    let run = |extra: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_wasm2rs"))
            .arg("--extract-core")
            .arg(&input)
            .args(extra)
            .output()
            .unwrap()
    };
    assert!(!run(&[]).status.success());
    let out = dir.join("out");
    assert!(!run(&[out.to_str().unwrap(), "10"]).status.success());
    assert!(!out.exists());
}

#[test]
fn an_extracted_module_transpiles_and_runs() {
    let wasm = wat::parse_str(COMPONENT).unwrap();
    let modules = extract_core_modules(&wasm).unwrap();
    assert!(transpile(&modules[1]).is_ok());
    common::compile_run_wasm("extract_core_run", &modules[2], "assert_eq!(func0(), 3);");
}

#[test]
fn the_cli_writes_each_module_to_the_directory() {
    let dir = std::env::temp_dir().join(format!("wasm2rs_extract_{}", std::process::id()));
    let out = dir.join("out");
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("c.wasm");
    std::fs::write(&input, wat::parse_str(COMPONENT).unwrap()).unwrap();
    let run = Command::new(env!("CARGO_BIN_EXE_wasm2rs"))
        .arg("--extract-core")
        .arg(&input)
        .arg(&out)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    for i in 0..3 {
        let bytes = std::fs::read(out.join(format!("module{i}.wasm"))).unwrap();
        assert!(dump(&bytes).is_ok());
    }
    assert!(!out.join("module3.wasm").exists());
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(stdout.contains("module0.wasm"), "{stdout}");
}
