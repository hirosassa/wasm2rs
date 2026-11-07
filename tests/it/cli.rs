//! Integration tests for the `wasm2rs` command-line entry point.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::arithmetic_side_effects,
    clippy::float_cmp,
    clippy::lossy_float_literal,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::unwrap_in_result,
    reason = "test code"
)]

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

/// Splitting into a directory also drops a `Cargo.toml` next to the sources so
/// the emitted crate builds out of the box. The strong oracle is a real
/// `cargo build --release` of that crate: it proves the manifest is valid TOML
/// Cargo accepts, that `[lib] path = "lib.rs"` wires the root and its chunk
/// submodules together, and that the size profile is honored — none of which a
/// string check on the manifest could establish.
#[test]
fn split_output_ships_a_buildable_cargo_manifest() {
    let (dir, input) = write_sample_wasm("manifest");
    let out_dir = dir.join("crate");

    let status = Command::new(wasm2rs_bin())
        .arg(&input)
        .arg(&out_dir)
        .arg("1")
        .status()
        .expect("run wasm2rs");
    assert!(status.success(), "wasm2rs exited nonzero");

    let manifest = std::fs::read_to_string(out_dir.join("Cargo.toml"))
        .expect("split output should include a Cargo.toml");
    assert!(
        manifest.contains("[profile.release]"),
        "manifest missing release profile:\n{manifest}"
    );

    // Build the emitted crate as its own package. `--offline` keeps the test
    // hermetic; the generated code depends only on `std`, so nothing is fetched.
    let build = Command::new(env!("CARGO"))
        .current_dir(&out_dir)
        .args(["build", "--release", "--offline"])
        .output()
        .expect("run cargo build");
    assert!(
        build.status.success(),
        "generated crate failed to build:\n--- stderr ---\n{}",
        String::from_utf8_lossy(&build.stderr),
    );
    assert!(
        out_dir.join("target/release").is_dir(),
        "release artifacts were not produced"
    );
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

/// A module that writes to a temp `.wasm` file for the `--unsafe-memory` tests:
/// one memory, one `i32.load`, one `i32.store`. Returns `(dir, input_path)`.
fn write_memory_wasm(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let wat = r#"(module
        (memory 1)
        (func (export "l") (param i32) (result i32) (i32.load (local.get 0)))
        (func (export "s") (param i32 i32) (i32.store (local.get 0) (local.get 1))))"#;
    let wasm = wat::parse_str(wat).expect("valid wat");
    let dir = std::env::temp_dir().join(format!("wasm2rs_cli_{name}"));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let input = dir.join("in.wasm");
    std::fs::write(&input, wasm).expect("write wasm");
    (dir, input)
}

/// `--unsafe-memory` makes the CLI emit unchecked (`get_unchecked`) memory
/// access; without it the default output stays bounds-checked.
#[test]
fn unsafe_memory_flag_emits_unchecked_access() {
    let (_dir, input) = write_memory_wasm("unsafe_flag");

    let safe = Command::new(wasm2rs_bin())
        .arg(&input)
        .output()
        .expect("run wasm2rs");
    assert!(safe.status.success(), "safe run exited nonzero");
    let safe_src = String::from_utf8_lossy(&safe.stdout);
    assert!(
        !safe_src.contains("get_unchecked"),
        "default output must stay bounds-checked",
    );

    let unsafe_out = Command::new(wasm2rs_bin())
        .arg("--unsafe-memory")
        .arg(&input)
        .output()
        .expect("run wasm2rs");
    assert!(unsafe_out.status.success(), "unsafe run exited nonzero");
    let unsafe_src = String::from_utf8_lossy(&unsafe_out.stdout);
    assert!(
        unsafe_src.contains("get_unchecked") && unsafe_src.contains("unsafe"),
        "--unsafe-memory must emit unchecked access:\n{unsafe_src}",
    );
}

/// A non-numeric `--split-dispatch` value is a usage error: nonzero exit, the
/// bad value named on stderr, and nothing written to the output directory.
#[test]
fn rejects_a_non_numeric_split_dispatch_value() {
    let (dir, input) = write_sample_wasm("bad_split_dispatch");
    let out_dir = dir.join("out_bad_split_dispatch");
    let _ = std::fs::remove_dir_all(&out_dir);

    let output = Command::new(wasm2rs_bin())
        .arg("--split-dispatch=lots")
        .arg(&input)
        .arg(&out_dir)
        .arg("1")
        .output()
        .expect("run wasm2rs");

    assert!(!output.status.success(), "expected a nonzero exit");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("invalid --split-dispatch value: lots"),
        "stderr was: {stderr}",
    );
    assert!(
        !out_dir.exists(),
        "no output should be written on a usage error"
    );
}

/// A chunk path that is already a directory makes the write fail: the CLI must
/// exit nonzero and say which file it could not write, not report success.
#[test]
fn a_failed_chunk_write_exits_nonzero_with_the_path() {
    let (dir, input) = write_sample_wasm("write_failure");
    let out_dir = dir.join("out_write_failure");
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(out_dir.join("lib.rs")).unwrap();

    let out = Command::new(wasm2rs_bin())
        .arg(&input)
        .arg(&out_dir)
        .arg("1")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(
        stderr.contains("cannot write") && stderr.contains("lib.rs"),
        "{stderr}"
    );
}

/// `--named-exports` adds by-name wrappers to the CLI output, and also in a split
/// crate (where they live in the root, which sees every chunk's functions).
#[test]
fn named_exports_flag_adds_wrappers_single_and_split() {
    let (dir, input) = write_sample_wasm("named_exports");
    let single = dir.join("out.rs");
    let status = Command::new(wasm2rs_bin())
        .arg("--named-exports")
        .arg(&input)
        .arg(&single)
        .status()
        .expect("run wasm2rs");
    assert!(status.success());
    let code = std::fs::read_to_string(&single).expect("read output");
    assert!(code.contains("pub fn a() -> i32 { func0() }"), "{code}");

    let out_dir = dir.join("split");
    let status = Command::new(wasm2rs_bin())
        .arg("--named-exports")
        .arg(&input)
        .arg(&out_dir)
        .arg("1")
        .status()
        .expect("run wasm2rs");
    assert!(status.success());
    let root = std::fs::read_to_string(out_dir.join("lib.rs")).expect("read lib.rs");
    for name in ["a", "b", "c"] {
        assert!(root.contains(&format!("pub fn {name}() -> i32")), "{root}");
    }
}

/// `--named-imports` names the `Imports` trait methods after the imports, in a
/// single file and in a split crate (where the trait lives in the root).
#[test]
fn named_imports_flag_names_the_trait_methods_single_and_split() {
    let wasm = wat::parse_str(
        r#"(module
            (import "env" "log" (func (param i32)))
            (func (export "f") (call 0 (i32.const 1))))"#,
    )
    .expect("valid wat");
    let dir = std::env::temp_dir().join("wasm2rs_cli_named_imports");
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let input = dir.join("in.wasm");
    std::fs::write(&input, wasm).expect("write wasm");

    let single = dir.join("out.rs");
    let status = Command::new(wasm2rs_bin())
        .arg("--named-imports")
        .arg(&input)
        .arg(&single)
        .status()
        .expect("run wasm2rs");
    assert!(status.success());
    let code = std::fs::read_to_string(&single).expect("read output");
    assert!(code.contains("fn env_log(&mut self, a0: i32);"), "{code}");
    assert!(!code.contains("fn import0("), "{code}");

    let out_dir = dir.join("split");
    let status = Command::new(wasm2rs_bin())
        .arg("--named-imports")
        .arg(&input)
        .arg(&out_dir)
        .arg("1")
        .status()
        .expect("run wasm2rs");
    assert!(status.success());
    let mut all = String::new();
    for entry in std::fs::read_dir(&out_dir).expect("read split dir") {
        let path = entry.expect("entry").path();
        if path.extension().is_some_and(|e| e == "rs") {
            all.push_str(&std::fs::read_to_string(path).expect("read chunk"));
        }
    }
    assert!(all.contains("fn env_log(&mut self, a0: i32);"), "{all}");
    assert!(all.contains("self.imports.env_log("), "{all}");
}

/// Run the CLI with `args`, assert it fails, and return its stderr.
fn failing_stderr(args: &[&std::path::Path]) -> String {
    let run = Command::new(wasm2rs_bin())
        .args(args)
        .output()
        .expect("run wasm2rs");
    assert!(!run.status.success(), "expected a nonzero exit");
    String::from_utf8_lossy(&run.stderr).into_owned()
}

/// A component with one core module, written next to a fresh scratch directory.
fn write_component(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let wasm = wat::parse_str(r#"(component (core module (func (export "a"))))"#).expect("wat");
    let dir = std::env::temp_dir().join(format!("wasm2rs_cli_{name}_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let input = dir.join("c.wasm");
    std::fs::write(&input, wasm).expect("write component");
    (dir, input)
}

#[test]
fn extract_core_reports_a_directory_it_cannot_create() {
    // The parent of the requested directory is a regular file, so the directory
    // cannot exist on any platform (and regardless of the user's privileges).
    let (dir, input) = write_component("extract_mkdir");
    let blocker = dir.join("blocker");
    std::fs::write(&blocker, b"not a directory").expect("write blocker");
    let out = blocker.join("modules");

    let stderr = failing_stderr(&["--extract-core".as_ref(), &input, &out]);
    assert!(
        stderr.starts_with(&format!("wasm2rs: cannot create {}: ", out.display())),
        "{stderr}"
    );
    assert!(blocker.is_file(), "the blocking file must be untouched");
}

#[test]
fn extract_core_reports_a_module_file_it_cannot_write() {
    // `module0.wasm` already exists as a directory, so writing the file fails.
    let (dir, input) = write_component("extract_write");
    let out = dir.join("out");
    std::fs::create_dir_all(out.join("module0.wasm")).expect("create blocking dir");

    let stderr = failing_stderr(&["--extract-core".as_ref(), &input, &out]);
    assert!(
        stderr.starts_with(&format!(
            "wasm2rs: cannot write {}: ",
            out.join("module0.wasm").display()
        )),
        "{stderr}"
    );
}

#[test]
fn an_output_file_in_a_missing_directory_is_reported() {
    let (dir, input) = write_sample_wasm("output_missing_dir");
    let output = dir.join("no-such-dir").join("out.rs");

    let stderr = failing_stderr(&[&input, &output]);
    assert!(
        stderr.starts_with(&format!("wasm2rs: cannot write {}: ", output.display())),
        "{stderr}"
    );
    assert!(!output.exists());
}

#[test]
fn a_split_output_directory_that_cannot_be_created_is_reported() {
    let (dir, input) = write_sample_wasm("split_mkdir");
    let blocker = dir.join("split_blocker");
    std::fs::write(&blocker, b"not a directory").expect("write blocker");
    let out = blocker.join("crate");

    let stderr = failing_stderr(&[&input, &out, "1".as_ref()]);
    assert!(
        stderr.starts_with(&format!("wasm2rs: cannot create {}: ", out.display())),
        "{stderr}"
    );
}
