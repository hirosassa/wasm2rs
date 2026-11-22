//! A real Cargo project end to end: its `build.rs` transpiles a module with a split and data side
//! files into `OUT_DIR` through `wasm2rs::build::run_to_out_dir`, `src/main.rs` `include!`s the
//! `lib.rs` there, and the built binary runs the result.

use std::path::{Path, PathBuf};
use std::process::Command;

const BUILD_RS: &str = r#"use wasm2rs::{SplitOptions, TranspileOptions, Transpiler};

fn main() {
    let t = Transpiler::new(TranspileOptions::default().with_named_exports(true))
        .with_split(SplitOptions { funcs_per_file: 1, max_bytes_per_file: 0 })
        .with_data_files(true);
    wasm2rs::build::run_to_out_dir(&t, "assets/foo.wasm", "foo").expect("transpile");
}
"#;

const MAIN_RS: &str = r#"mod foo {
    include!(concat!(env!("OUT_DIR"), "/foo/lib.rs"));
}

fn main() {
    let mut inst = foo::Instance::new();
    println!("{}", inst.add_one(41));
    println!("{}", env!("OUT_DIR"));
}
"#;

#[test]
fn a_build_script_writes_split_output_and_data_files_that_the_crate_includes() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let dir = std::env::temp_dir().join(format!("wasm2rs-build-split-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::create_dir_all(dir.join("assets")).unwrap();
    let manifest = format!(
        r#"[package]
name = "project-split"
version = "0.0.0"
edition = "2024"
build = "build.rs"

[workspace]

[build-dependencies]
wasm2rs = {{ path = {root:?}, default-features = false }}
"#
    );
    std::fs::write(dir.join("Cargo.toml"), manifest).unwrap();
    std::fs::copy(root.join("Cargo.lock"), dir.join("Cargo.lock")).unwrap();
    std::fs::write(dir.join("build.rs"), BUILD_RS).unwrap();
    std::fs::write(dir.join("src/main.rs"), MAIN_RS).unwrap();
    // Two functions so the split has several chunks; a 6000-byte segment so a `data_0.bin` is written.
    let wat = format!(
        r#"(module (memory 1) (data (i32.const 0) "{}")
  (func (export "add-one") (param i32) (result i32) local.get 0 i32.const 1 i32.add)
  (func (export "two") (result i32) i32.const 2))"#,
        "ab".repeat(3000)
    );
    std::fs::write(dir.join("assets/foo.wasm"), wat::parse_str(wat).unwrap()).unwrap();

    let target: PathBuf = std::env::temp_dir().join("wasm2rs-build-split-target");
    let build = Command::new(env!("CARGO"))
        .current_dir(&dir)
        .args(["build", "--quiet", "--offline"])
        .env("CARGO_TARGET_DIR", &target)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(target.join("debug/project-split"))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout).into_owned();
    let mut lines = stdout.lines();
    assert_eq!(lines.next(), Some("42"));
    let out_foo = Path::new(lines.next().expect("OUT_DIR line")).join("foo");

    // The side files are where `lib.rs` expects them.
    assert!(out_foo.join("lib.rs").is_file());
    assert!(out_foo.join("funcs_0.rs").is_file());
    assert!(out_foo.join("data_0.bin").is_file());
    let _ = std::fs::remove_dir_all(dir);
}
