//! `wasm2rs::build`: transpile a `.wasm` file into a build script's `OUT_DIR`.

use std::path::PathBuf;

use wasm2rs::build::{
    BuildError, transpile_to_dir, transpile_to_dir_with_options, transpile_to_out_dir,
    transpile_to_out_dir_with_options,
};
use wasm2rs::{TranspileOptions, transpile, transpile_with_options};

const WAT: &str = r#"(module
  (func (export "add-one") (param i32) (result i32)
    local.get 0 i32.const 1 i32.add))"#;

/// A fresh scratch directory holding `foo.wasm` built from [`WAT`].
fn scratch(name: &str) -> (PathBuf, PathBuf, Vec<u8>) {
    let dir = std::env::temp_dir().join(format!("wasm2rs_build_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let wasm = wat::parse_str(WAT).unwrap();
    let path = dir.join("foo.wasm");
    std::fs::write(&path, &wasm).unwrap();
    (dir, path, wasm)
}

#[test]
fn writes_the_transpiled_source_into_the_directory() {
    let (dir, wasm_path, wasm) = scratch("plain");
    let out = transpile_to_dir(&dir, &wasm_path, "foo.rs").unwrap();
    assert_eq!(out, dir.join("foo.rs"));
    assert_eq!(
        std::fs::read_to_string(&out).unwrap(),
        transpile(&wasm).unwrap()
    );
}

#[test]
fn options_are_applied() {
    let (dir, wasm_path, wasm) = scratch("opts");
    let opts = TranspileOptions::default().with_named_exports(true);
    let out = transpile_to_dir_with_options(&dir, &wasm_path, "foo.rs", &opts).unwrap();
    let got = std::fs::read_to_string(out).unwrap();
    assert_eq!(got, transpile_with_options(&wasm, &opts).unwrap());
    assert!(got.contains("pub fn add_one"));
}

#[test]
fn a_missing_wasm_file_is_an_io_error() {
    let (dir, _, _) = scratch("missing");
    let err = transpile_to_dir(&dir, dir.join("nope.wasm"), "foo.rs").unwrap_err();
    assert!(matches!(err, BuildError::Io { .. }), "{err}");
    assert!(err.to_string().contains("nope.wasm"), "{err}");
}

#[test]
fn invalid_wasm_is_a_transpile_error() {
    let (dir, wasm_path, _) = scratch("invalid");
    std::fs::write(&wasm_path, b"not wasm").unwrap();
    let err = transpile_to_dir(&dir, &wasm_path, "foo.rs").unwrap_err();
    assert!(matches!(err, BuildError::Transpile(_)), "{err}");
}

#[test]
fn out_dir_wrappers_require_out_dir() {
    // Cargo sets `OUT_DIR` for build scripts only, not for the test process.
    if std::env::var_os("OUT_DIR").is_some() {
        return;
    }
    let (_, wasm_path, _) = scratch("no_out_dir");
    let err = transpile_to_out_dir(&wasm_path, "foo.rs").unwrap_err();
    assert!(matches!(err, BuildError::MissingOutDir), "{err}");
    assert!(err.to_string().contains("OUT_DIR"), "{err}");
    let err = transpile_to_out_dir_with_options(&wasm_path, "foo.rs", &TranspileOptions::default())
        .unwrap_err();
    assert!(matches!(err, BuildError::MissingOutDir), "{err}");
}

#[test]
fn errors_display_and_expose_their_source() {
    use std::error::Error;

    let (dir, wasm_path, _) = scratch("errors");
    assert!(BuildError::MissingOutDir.source().is_none());

    let io = transpile_to_dir(&dir, dir.join("nope.wasm"), "foo.rs").unwrap_err();
    assert!(io.source().is_some());

    std::fs::write(&wasm_path, b"not wasm").unwrap();
    let parse = transpile_to_dir(&dir, &wasm_path, "foo.rs").unwrap_err();
    assert!(parse.source().is_some());
    assert!(
        parse.to_string().contains("failed to parse wasm"),
        "{parse}"
    );
}

#[test]
fn an_unwritable_output_is_an_io_error() {
    let (dir, wasm_path, _) = scratch("unwritable");
    let err = transpile_to_dir(dir.join("no_such_dir"), &wasm_path, "foo.rs").unwrap_err();
    assert!(matches!(err, BuildError::Io { .. }), "{err}");
    assert!(err.to_string().contains("foo.rs"), "{err}");
}

// --- components ---

mod component {
    use wasm2rs::build::{BuildError, transpile_component_to_dir, transpile_component_to_out_dir};
    use wasm2rs::{ComponentOptions, transpile_component};

    use crate::component_input::build_and_run;
    use crate::wasip2_run_example::{CORE, WORLD, component_of};

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("wasm2rs_build_c_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn writes_the_component_source_named_after_the_file_stem() {
        let dir = scratch("write");
        let wasm = component_of(WORLD, CORE);
        let path = dir.join("hello.wasm");
        std::fs::write(&path, &wasm).unwrap();
        let out = transpile_component_to_dir(&dir, &path, &ComponentOptions::default()).unwrap();
        assert_eq!(out, dir.join("hello.rs"));
        assert_eq!(
            std::fs::read_to_string(out).unwrap(),
            transpile_component(&wasm, &ComponentOptions::default()).unwrap()
        );
    }

    #[test]
    fn the_written_source_builds_with_the_wasi_host_and_runs() {
        let dir = scratch("run");
        let path = dir.join("hello.wasm");
        std::fs::write(&path, component_of(WORLD, CORE)).unwrap();
        let out = transpile_component_to_dir(&dir, &path, &ComponentOptions::default()).unwrap();
        let code = std::fs::read_to_string(out).unwrap();
        let out = build_and_run(&code, "build_helper_component_run", "hi");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "hi");
        assert!(out.status.success());
    }

    #[test]
    fn a_missing_file_is_an_io_error() {
        let dir = scratch("missing");
        let err =
            transpile_component_to_dir(&dir, dir.join("nope.wasm"), &ComponentOptions::default())
                .unwrap_err();
        assert!(matches!(err, BuildError::Io { .. }), "{err}");
        assert!(err.to_string().contains("nope.wasm"), "{err}");
    }

    #[test]
    fn a_core_module_is_a_transpile_error() {
        let dir = scratch("core");
        let path = dir.join("core.wasm");
        std::fs::write(
            &path,
            wat::parse_str("(module (func (export \"f\")))").unwrap(),
        )
        .unwrap();
        let err =
            transpile_component_to_dir(&dir, &path, &ComponentOptions::default()).unwrap_err();
        assert!(matches!(err, BuildError::Transpile(_)), "{err}");
        assert!(!dir.join("core.rs").exists(), "nothing is written on error");
    }

    #[test]
    fn the_out_dir_wrapper_requires_out_dir() {
        if std::env::var_os("OUT_DIR").is_some() {
            return;
        }
        let dir = scratch("no_out_dir");
        let path = dir.join("hello.wasm");
        std::fs::write(&path, component_of(WORLD, CORE)).unwrap();
        let err = transpile_component_to_out_dir(&path, &ComponentOptions::default()).unwrap_err();
        assert!(matches!(err, BuildError::MissingOutDir), "{err}");
    }
}
