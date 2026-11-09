//! A real Cargo project end to end: its `build.rs` calls `transpile_component_to_out_dir`,
//! `wasm2rs_macros::include_component!` includes the result, and the built binary runs the
//! component through `wasm2rs-wasi`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use super::wasip2_run_example::component;

const BUILD_RS: &str = r#"fn main() {
    wasm2rs::build::transpile_component_to_out_dir(
        "assets/run.wasm",
        &wasm2rs::ComponentOptions::default(),
    )
    .expect("transpile the component");
}
"#;

const MAIN_RS: &str = r#"mod run {
    wasm2rs_macros::include_component!("NAME");
}

fn main() {
    let args = std::env::args().collect();
    let host = wasm2rs_wasi::WasiHost::new(Vec::new()).with_arguments(args);
    let mut inst = run::Instance::new(run::Glue(host));
    let ok = run::Exports::export_wasi_cli_run_0_2_0_run(&mut inst).is_ok();
    std::process::exit(if ok { 0 } else { 1 });
}
"#;

/// Lays out a project whose `src/main.rs` includes `include_name`, and builds it. One target
/// directory is shared by all projects so the dependencies compile once; the package name
/// carries `tag` because cargo's artifact hash would otherwise be the same for both projects.
fn build_project(tag: &str, include_name: &str) -> (PathBuf, Output) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let dir = std::env::temp_dir().join(format!("wasm2rs-include-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::create_dir_all(dir.join("assets")).unwrap();
    let manifest = format!(
        r#"[package]
name = "project-{tag}"
version = "0.0.0"
edition = "2024"
build = "build.rs"

[workspace]

[dependencies]
wasm2rs-wasi = {{ path = {wasi:?} }}
wasm2rs-macros = {{ path = {macros:?} }}

[build-dependencies]
wasm2rs = {{ path = {root:?}, default-features = false }}
"#,
        wasi = root.join("wasm2rs-wasi"),
        macros = root.join("wasm2rs-macros"),
        root = root,
    );
    std::fs::write(dir.join("Cargo.toml"), manifest).unwrap();
    std::fs::write(dir.join("build.rs"), BUILD_RS).unwrap();
    std::fs::write(dir.join("assets/run.wasm"), component()).unwrap();
    std::fs::write(
        dir.join("src/main.rs"),
        MAIN_RS.replace("NAME", include_name),
    )
    .unwrap();
    let out = Command::new(env!("CARGO"))
        .current_dir(&dir)
        .args(["build", "--quiet"])
        .env(
            "CARGO_TARGET_DIR",
            std::env::temp_dir().join("wasm2rs-include-target"),
        )
        .output()
        .unwrap();
    (dir, out)
}

#[test]
fn a_build_script_project_includes_and_runs_a_component() {
    let (dir, build) = build_project("ok", "run");
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let bin = std::env::temp_dir().join("wasm2rs-include-target/debug/project-ok");
    let out = Command::new(bin).args(["hello"]).output().unwrap();
    // argv[0] is the binary, so `run` (which prints argv[1]) prints `hello`
    assert_eq!(String::from_utf8_lossy(&out.stdout), "hello");
    assert!(out.status.success());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_wasm_extension_is_a_compile_error_with_the_fix() {
    let (dir, build) = build_project("ext", "run.wasm");
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(!build.status.success(), "{stderr}");
    assert!(
        stderr.contains("`run.wasm` has the `.wasm` extension"),
        "{stderr}"
    );
    assert!(stderr.contains("include_component!(\"run\")"), "{stderr}");
    let _ = std::fs::remove_dir_all(dir);
}
