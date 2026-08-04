//! Integration tests for the `wasm2rs` command-line entry point.

use std::process::Command;

/// The compiled `wasm2rs` binary under test.
fn wasm2rs_bin() -> &'static str {
    env!("CARGO_BIN_EXE_wasm2rs")
}

/// A three-function module, written to a temp `.wasm` file whose path is
/// returned alongside its containing directory.
fn write_sample_wasm(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let wat = r#"(module
        (func (export "a") (result i32) i32.const 1)
        (func (export "b") (result i32) i32.const 2)
        (func (export "c") (result i32) i32.const 3))"#;
    let wasm = wat::parse_str(wat).expect("valid wat");
    let dir = std::env::temp_dir().join(format!("wasm2rs_cli_{name}"));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let input = dir.join("in.wasm");
    std::fs::write(&input, wasm).expect("write wasm");
    (dir, input)
}

/// With `funcs_per_file = 1` and a target directory, the CLI writes one chunk
/// file per function plus the `lib.rs` root.
#[test]
fn splits_into_a_directory_of_files() {
    let (dir, input) = write_sample_wasm("split");
    let out_dir = dir.join("out");

    let status = Command::new(wasm2rs_bin())
        .arg(&input)
        .arg(&out_dir)
        .arg("1")
        .status()
        .expect("run wasm2rs");
    assert!(status.success(), "wasm2rs exited nonzero");

    for name in ["lib.rs", "funcs_0.rs", "funcs_1.rs", "funcs_2.rs"] {
        let path = out_dir.join(name);
        let code = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("expected {name} to exist: {e}"));
        assert!(!code.is_empty(), "{name} is empty");
    }
    // The root declares every chunk module.
    let root = std::fs::read_to_string(out_dir.join("lib.rs")).unwrap();
    assert!(
        root.contains("mod funcs_2;"),
        "root missing chunk decl:\n{root}"
    );
}

/// Without a `funcs_per_file` argument the CLI writes a single output file
/// identical to the library's `transpile`.
#[test]
fn single_file_output_matches_library() {
    let (dir, input) = write_sample_wasm("single");
    let out_file = dir.join("out.rs");

    let status = Command::new(wasm2rs_bin())
        .arg(&input)
        .arg(&out_file)
        .status()
        .expect("run wasm2rs");
    assert!(status.success(), "wasm2rs exited nonzero");

    let written = std::fs::read_to_string(&out_file).expect("read output");
    let wasm = std::fs::read(&input).unwrap();
    let expected = wasm2rs::transpile(&wasm).expect("transpile ok");
    assert_eq!(written, expected);
}

/// With no output path the CLI writes the transpiled Rust to stdout, byte-for-byte
/// identical to the library's `transpile`.
#[test]
fn no_output_arg_writes_to_stdout() {
    let (_dir, input) = write_sample_wasm("stdout");

    let out = Command::new(wasm2rs_bin())
        .arg(&input)
        .output()
        .expect("run wasm2rs");
    assert!(out.status.success(), "wasm2rs exited nonzero");

    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let wasm = std::fs::read(&input).unwrap();
    let expected = wasm2rs::transpile(&wasm).expect("transpile ok");
    assert_eq!(stdout, expected);
}

/// With no arguments at all the CLI fails and prints the usage string, rather
/// than panicking or exiting successfully.
#[test]
fn missing_input_argument_fails_with_usage() {
    let out = Command::new(wasm2rs_bin()).output().expect("run wasm2rs");
    assert!(!out.status.success(), "expected a nonzero exit");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("usage: wasm2rs"),
        "expected usage on stderr, got: {stderr:?}",
    );
}

/// A non-numeric `funcs_per_file` argument is rejected with a message naming the
/// offending argument, not a panic from an unwrapped `parse`.
#[test]
fn non_numeric_split_argument_is_rejected() {
    let (dir, input) = write_sample_wasm("bad_arg");
    let out_dir = dir.join("out");

    let out = Command::new(wasm2rs_bin())
        .arg(&input)
        .arg(&out_dir)
        .arg("not-a-number")
        .output()
        .expect("run wasm2rs");
    assert!(!out.status.success(), "expected a nonzero exit");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("invalid funcs_per_file"),
        "expected an invalid-argument message, got: {stderr:?}",
    );
}

/// A missing input *file* (as opposed to a missing argument) fails with a read
/// error that names the path.
#[test]
fn unreadable_input_file_fails_with_a_read_error() {
    let missing = std::env::temp_dir().join("wasm2rs_definitely_missing_input.wasm");
    let _ = std::fs::remove_file(&missing);

    let out = Command::new(wasm2rs_bin())
        .arg(&missing)
        .output()
        .expect("run wasm2rs");
    assert!(!out.status.success(), "expected a nonzero exit");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("cannot read"),
        "expected a read error on stderr, got: {stderr:?}",
    );
}
