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

/// Shared by all projects so the dependencies compile once.
fn target_dir() -> PathBuf {
    std::env::temp_dir().join("wasm2rs-include-target")
}

/// Lays out a project whose `src/main.rs` includes `include_name`, and builds it. The package
/// name carries `tag` because cargo's artifact hash would otherwise be the same for both
/// projects in the shared [`target_dir`].
fn build_project(tag: &str, include_name: &str) -> (PathBuf, Output) {
    build_project_with_main(tag, &MAIN_RS.replace("NAME", include_name))
}

/// Like [`build_project`], with the given `src/main.rs`.
fn build_project_with_main(tag: &str, main_rs: &str) -> (PathBuf, Output) {
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
    // The workspace's lockfile covers every dependency here: the versions stay the pinned ones.
    std::fs::copy(root.join("Cargo.lock"), dir.join("Cargo.lock")).unwrap();
    std::fs::write(dir.join("build.rs"), BUILD_RS).unwrap();
    std::fs::write(dir.join("assets/run.wasm"), component()).unwrap();
    std::fs::write(dir.join("src/main.rs"), main_rs).unwrap();
    let out = Command::new(env!("CARGO"))
        .current_dir(&dir)
        .args(["build", "--quiet", "--offline"])
        .env("CARGO_TARGET_DIR", target_dir())
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
    let bin = target_dir().join("debug/project-ok");
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

#[test]
fn include_wasm_with_malformed_arguments_is_a_usage_compile_error() {
    // Each call is malformed in a different place: a punctuation other than a comma between
    // the arguments, a non-identifier option, and a path that is not a plain literal.
    for (tag, args) in [
        ("sep", r#""assets/run.wasm"; opt"#),
        ("opt", r#""assets/run.wasm", 1"#),
        ("lit", "assets"),
    ] {
        let main = format!("mod m {{ wasm2rs_macros::include_wasm!({args}); }}\nfn main() {{}}\n");
        let (dir, build) = build_project_with_main(tag, &main);
        let stderr = String::from_utf8_lossy(&build.stderr);
        assert!(!build.status.success(), "{args}: {stderr}");
        assert!(
            stderr.contains("expected `include_wasm!(\"path.wasm\" [, option]*)`"),
            "{args}: {stderr}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
