//! Shared end-to-end test helpers.
//!
//! Every helper follows the same pipeline: assemble WAT to wasm (`wat`),
//! transpile it to Rust (`wasm2rs::transpile`), compile the result with
//! `rustc -D warnings`, and run it. `compile_run` asserts the program exits
//! successfully (so its own `assert!`s held); `expect_trap` asserts it aborts;
//! `expect_trap_with` additionally checks the panic message so a trap test pins
//! *why* it trapped, not merely that it did.
//!
//! Not every test binary uses every helper, so the module allows dead code.
#![allow(dead_code)]

use std::process::Command;

use wasm2rs::{SourceFile, SplitOptions, transpile_split};

/// Transpile `wat` into a multi-file crate with the given `funcs_per_file`,
/// returning every emitted [`SourceFile`] in emission order.
pub fn transpile_files(wat: &str, funcs_per_file: usize) -> Vec<SourceFile> {
    transpile_files_capped(wat, funcs_per_file, 0)
}

/// Like [`transpile_files`] but also enforces a `max_bytes_per_file` chunk cap.
pub fn transpile_files_capped(
    wat: &str,
    funcs_per_file: usize,
    max_bytes_per_file: usize,
) -> Vec<SourceFile> {
    let wasm = wat::parse_str(wat).expect("valid wat");
    let opts = SplitOptions {
        funcs_per_file,
        max_bytes_per_file,
    };
    let mut files = Vec::new();
    transpile_split(&wasm, &opts, |f| {
        files.push(f);
        Ok(())
    })
    .expect("transpile ok");
    files
}

/// Transpile `wat` into a multi-file crate (splitting at `funcs_per_file`), write
/// the files to a temp dir, append `fn main {{ main_body }}` to the crate root
/// (`lib.rs`), compile the whole crate with `rustc -D warnings`, run it, and
/// assert it exits 0. Returns the emitted file names in order so a test can also
/// assert the split shape.
pub fn compile_run_split(
    name: &str,
    wat: &str,
    funcs_per_file: usize,
    main_body: &str,
) -> Vec<String> {
    let files = transpile_files(wat, funcs_per_file);
    let names: Vec<String> = files.iter().map(|f| f.name.clone()).collect();

    let dir = std::env::temp_dir().join(format!("wasm2rs_{name}"));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    for f in &files {
        let mut code = f.code.clone();
        // The crate root is turned into a runnable binary by appending `main`.
        if f.name == "lib.rs" {
            code.push_str(&format!("\nfn main() {{\n{main_body}\n}}\n"));
        }
        std::fs::write(dir.join(&f.name), code).expect("write generated file");
    }

    let bin = dir.join(if cfg!(windows) { "gen.exe" } else { "gen" });
    let out = Command::new("rustc")
        .current_dir(&dir)
        .arg("lib.rs")
        .arg("--edition")
        .arg("2021")
        .arg("-D")
        .arg("warnings")
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("run rustc");
    assert!(
        out.status.success(),
        "multi-file crate failed to compile ({name}):\n--- files ---\n{}\n--- stderr ---\n{}",
        names.join(", "),
        String::from_utf8_lossy(&out.stderr),
    );

    let run = Command::new(&bin).status().expect("run generated binary");
    assert!(
        run.success(),
        "generated program assertions failed:\n{name}"
    );
    names
}

/// Compile `program` (a complete Rust source file) with `rustc -D warnings`
/// into a temp-dir binary and return its path. Panics with the compiler's
/// stderr if the generated code does not build.
fn build(name: &str, program: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("wasm2rs_{name}"));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let src = dir.join("gen.rs");
    let bin = dir.join(if cfg!(windows) { "gen.exe" } else { "gen" });
    std::fs::write(&src, program).expect("write generated source");

    let out = Command::new("rustc")
        // Isolate each parallel rustc's codegen-unit temp objects per test dir.
        .current_dir(&dir)
        .arg(&src)
        .arg("--edition")
        .arg("2021")
        .arg("-D")
        .arg("warnings")
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("run rustc");
    assert!(
        out.status.success(),
        "generated code failed to compile:\n{program}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&out.stderr),
    );
    bin
}

/// Transpile `wat` and wrap the generated Rust with `fn main() {{ main_body }}`.
fn program_for(wat: &str, main_body: &str) -> String {
    let wasm = wat::parse_str(wat).expect("valid wat");
    let generated = wasm2rs::transpile(&wasm).expect("transpile ok");
    format!("{generated}\nfn main() {{\n{main_body}\n}}\n")
}

/// Transpile a single module, run `main_body`, and assert it exits 0.
pub fn compile_run(name: &str, wat: &str, main_body: &str) {
    let bin = build(name, &program_for(wat, main_body));
    let run = Command::new(&bin).status().expect("run generated binary");
    assert!(
        run.success(),
        "generated program assertions failed:\n{name}"
    );
}

/// Transpile `wat` and append `trailer` verbatim — used when the test needs
/// module-level definitions (e.g. a host `impl Imports`) in addition to a
/// `fn main`. Asserts the program exits 0.
pub fn compile_run_raw(name: &str, wat: &str, trailer: &str) {
    let wasm = wat::parse_str(wat).expect("valid wat");
    let generated = wasm2rs::transpile(&wasm).expect("transpile ok");
    let program = format!("{generated}\n{trailer}\n");
    let bin = build(name, &program);
    let run = Command::new(&bin).status().expect("run generated binary");
    assert!(
        run.success(),
        "generated program assertions failed:\n{name}"
    );
}

/// Transpile several modules into sibling `pub mod`s (for cross-module linking
/// scenarios), run `main_body`, and assert it exits 0.
pub fn compile_run_multi(name: &str, modules: &[(&str, &str)], main_body: &str) {
    let mut program = String::new();
    for (modname, wat) in modules {
        let wasm = wat::parse_str(wat).expect("valid wat");
        let generated = wasm2rs::transpile(&wasm).expect("transpile ok");
        program.push_str(&format!("pub mod {modname} {{\n{generated}\n}}\n"));
    }
    program.push_str(&format!("fn main() {{\n{main_body}\n}}\n"));

    let bin = build(name, &program);
    let run = Command::new(&bin).status().expect("run generated binary");
    assert!(
        run.success(),
        "generated program assertions failed:\n{name}"
    );
}

/// Assert that running `main_body` traps (the process aborts with a nonzero
/// exit), without inspecting the reason.
pub fn expect_trap(name: &str, wat: &str, main_body: &str) {
    let bin = build(name, &program_for(wat, main_body));
    let run = Command::new(&bin).output().expect("run generated binary");
    assert!(
        !run.status.success(),
        "expected a trap but the program exited successfully:\n{name}",
    );
}

/// Assert that running `main_body` traps *and* that the panic output on stderr
/// contains `needle`, pinning the trap reason.
pub fn expect_trap_with(name: &str, wat: &str, main_body: &str, needle: &str) {
    let bin = build(name, &program_for(wat, main_body));
    let run = Command::new(&bin).output().expect("run generated binary");
    assert!(
        !run.status.success(),
        "expected a trap but the program exited successfully:\n{name}",
    );
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        stderr.contains(needle),
        "trap reason mismatch for {name}: expected stderr to contain {needle:?}\n--- stderr ---\n{stderr}",
    );
}
