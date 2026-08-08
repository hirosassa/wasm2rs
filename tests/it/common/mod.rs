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
#![allow(dead_code, reason = "not every test binary uses every helper")]

use std::hash::{Hash, Hasher};
use std::process::Command;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use wasm2rs::{SourceFile, SplitOptions, transpile_split};

/// The exact `rustc` flags every single-file build uses. Kept in one place so
/// the cache key covers them: changing a flag must invalidate cached binaries.
const RUSTC_FLAGS: &[&str] = &[
    "--edition",
    "2024",
    "-D",
    "warnings",
    "-C",
    "debuginfo=0",
    "-C",
    "opt-level=0",
];

/// `rustc --version`, resolved once. Folded into the cache key so a toolchain
/// upgrade invalidates every cached binary rather than silently reusing stale
/// output from an older compiler.
fn rustc_version() -> &'static str {
    static VERSION: OnceLock<String> = OnceLock::new();
    VERSION.get_or_init(|| {
        let out = Command::new("rustc")
            .arg("--version")
            .output()
            .expect("query rustc version");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    })
}

/// Content-addressed cache key for a generated program: identical `(program,
/// flags, toolchain)` triples map to one key, distinct ones (almost surely) to
/// distinct keys. `DefaultHasher` uses fixed keys, so the digest is stable
/// across processes and runs — a persistent on-disk cache relies on that.
fn cache_key(program: &str) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    program.hash(&mut h);
    RUSTC_FLAGS.hash(&mut h);
    rustc_version().hash(&mut h);
    format!("gen-{:016x}", h.finish())
}

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
        .arg("2024")
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
/// into a binary and return its path. Panics with the compiler's stderr if the
/// generated code does not build.
///
/// Builds are memoized by content hash: because the transpiler is
/// deterministic, re-running the suite recompiles byte-identical programs, so a
/// content-addressed cache turns the second run of each test into a no-op rustc
/// invocation. `name` only labels the per-build working directory for
/// readability; it does not affect the cache key.
pub fn build(name: &str, program: &str) -> std::path::PathBuf {
    let cache_dir = std::env::temp_dir().join("wasm2rs-testcache");
    std::fs::create_dir_all(&cache_dir).expect("create cache dir");
    let exe = if cfg!(windows) { ".exe" } else { "" };
    let bin = cache_dir.join(format!("{}{exe}", cache_key(program)));
    if bin.exists() {
        return bin;
    }

    // Cache miss: compile in a per-call working dir so parallel rustc processes
    // never share codegen-unit temp objects, then atomically publish the binary
    // into the shared cache. Concurrent misses of the same key both compile and
    // rename; the last rename wins and every waiter observes a complete binary.
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let uniq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let work = cache_dir.join(format!("work-{name}-{}-{uniq}", std::process::id()));
    std::fs::create_dir_all(&work).expect("create work dir");
    let src = work.join("gen.rs");
    let staged = work.join(format!("gen{exe}"));
    std::fs::write(&src, program).expect("write generated source");

    let mut cmd = Command::new("rustc");
    cmd.current_dir(&work).arg(&src);
    cmd.args(RUSTC_FLAGS);
    cmd.arg("-o").arg(&staged);
    let out = cmd.output().expect("run rustc");
    assert!(
        out.status.success(),
        "generated code failed to compile:\n{program}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&out.stderr),
    );
    std::fs::rename(&staged, &bin).expect("publish binary into cache");
    std::fs::remove_dir_all(&work).ok();
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
