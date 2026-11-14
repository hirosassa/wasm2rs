//! Shared end-to-end test helpers.
//!
//! Every helper follows the same pipeline: assemble WAT to wasm (`wat`),
//! transpile it to Rust (`wasm2rs::transpile`), compile the result with
//! `rustc -D warnings`, and run it. `compile_run` asserts the program exits
//! successfully (so its own `assert!`s held); `expect_trap` asserts it aborts;
//! `expect_trap_with` additionally checks the panic message so a trap test pins
//! *why* it trapped, not merely that it did.
#![allow(dead_code, reason = "not every test binary uses every helper")]

pub mod prune;

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
fn cache_key(program: &str, wasi: Option<&WasiLib>) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    program.hash(&mut h);
    wasi.map(|lib| lib.digest).hash(&mut h);
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

    let dir = scratch_dir(&format!("split-{name}"));
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
    // only on success: a failed build's directory is left behind to inspect
    std::fs::remove_dir_all(&dir).ok();
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
    build_with(name, program, None)
}

/// Like [`build`], with the `wasm2rs-wasi` library linked as `wasm2rs_wasi`.
pub fn build_with_wasi(name: &str, program: &str) -> std::path::PathBuf {
    build_with(name, program, Some(wasi_lib()))
}

/// The `wasm2rs-wasi` rlib with its default features, built by a nested `cargo build` into a
/// target directory of its own. The crate depends on `getrandom` (feature `os-random`), and a
/// bare `rustc` call cannot resolve dependencies, so cargo builds the library and its
/// dependencies; the generated programs then link it with `-L dependency=` on the same
/// directory. The ambient `RUSTFLAGS` and wrappers are removed from the nested build, so
/// coverage instrumentation cannot leak into the rlib.
struct WasiLib {
    rlib: std::path::PathBuf,
    /// The `.rmeta` that goes with it, if cargo built one that the rlib needs.
    rmeta: Option<std::path::PathBuf>,
    /// The directories holding the rlibs of the library's dependencies.
    deps: Vec<std::path::PathBuf>,
    /// Hash of the rlib's bytes and the toolchain: part of the cache key of every program
    /// linked against it, so a change to the host invalidates cached binaries.
    digest: u64,
}

/// The `wasm2rs-wasi` rlib and the directories of all the rlibs that cargo built, from
/// `cargo build --message-format=json` output (their places depend on the cargo version).
fn wasi_artifacts(
    json_lines: &str,
) -> Option<(
    std::path::PathBuf,
    Option<std::path::PathBuf>,
    Vec<std::path::PathBuf>,
)> {
    let mut lib = None;
    let mut rmeta = None;
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    for line in json_lines.lines() {
        if !line.contains("\"reason\":\"compiler-artifact\"") {
            continue;
        }
        let Some(list) = line
            .split("\"filenames\":[")
            .nth(1)
            .and_then(|l| l.split(']').next())
        else {
            continue;
        };
        if line.contains("\"name\":\"wasm2rs_wasi\"") {
            // newer cargo leaves only a metadata stub in the rlib: the .rmeta must be given too
            rmeta = list
                .split('"')
                .find(|f| f.ends_with(".rmeta"))
                .map(std::path::PathBuf::from);
        }
        for rlib in list.split('"').filter(|f| f.ends_with(".rlib")) {
            let rlib = std::path::PathBuf::from(rlib);
            if line.contains("\"name\":\"wasm2rs_wasi\"") {
                lib = Some(rlib.clone());
            }
            if let Some(dir) = rlib.parent().filter(|d| !dirs.iter().any(|x| x == d)) {
                dirs.push(dir.to_path_buf());
            }
        }
    }
    Some((lib?, rmeta, dirs))
}

fn wasi_lib() -> &'static WasiLib {
    static LIB: OnceLock<WasiLib> = OnceLock::new();
    LIB.get_or_init(|| {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let cache_dir = cache_dir();
        let target = checkout_dir().join("wasi-target");
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let out = Command::new(cargo)
            .args([
                "build",
                "-p",
                "wasm2rs-wasi",
                "--message-format=json",
                "--manifest-path",
            ])
            .arg(root.join("Cargo.toml"))
            .arg("--target-dir")
            .arg(&target)
            .env_remove("RUSTFLAGS")
            .env_remove("CARGO_ENCODED_RUSTFLAGS")
            .env_remove("CARGO_BUILD_RUSTFLAGS")
            .env_remove("RUSTC_WRAPPER")
            .env_remove("RUSTC_WORKSPACE_WRAPPER")
            .env_remove("LLVM_PROFILE_FILE")
            .output()
            .expect("run cargo build for wasm2rs-wasi");
        assert!(
            out.status.success(),
            "wasm2rs-wasi failed to build:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        // Where cargo put the rlib is cargo's business (a separate build directory holds the
        // intermediate files in newer versions): ask for it, and look for the dependencies
        // beside it.
        let (built, built_rmeta, deps) = wasi_artifacts(&String::from_utf8_lossy(&out.stdout))
            .expect("cargo reported no wasm2rs-wasi rlib");
        // Link a content-addressed copy, so the digest matches the bytes that are linked
        // even if cargo replaces the file later.
        let bytes = std::fs::read(&built).expect("read wasm2rs-wasi rlib");
        let meta_bytes = built_rmeta
            .as_ref()
            .map(|p| std::fs::read(p).expect("read wasm2rs-wasi rmeta"));
        let mut h = std::collections::hash_map::DefaultHasher::new();
        bytes.hash(&mut h);
        meta_bytes.hash(&mut h);
        rustc_version().hash(&mut h);
        let digest = h.finish();
        let publish = |ext: &str, bytes: &[u8]| {
            let path = cache_dir.join(format!("libwasm2rs_wasi-{digest:016x}.{ext}"));
            if !path.exists() {
                let staged = cache_dir.join(format!(
                    "libwasm2rs_wasi-{digest:016x}-{}.{ext}.tmp",
                    std::process::id()
                ));
                std::fs::write(&staged, bytes).expect("stage wasm2rs-wasi artifact");
                std::fs::rename(&staged, &path).expect("publish artifact into cache");
            }
            path
        };
        let rlib = publish("rlib", &bytes);
        let rmeta = meta_bytes.map(|m| publish("rmeta", &m));
        WasiLib {
            rlib,
            rmeta,
            deps,
            digest,
        }
    })
}

/// Where the cache lives: `$WASM2RS_TESTCACHE_DIR` when set and non-empty (CI points it at a
/// directory it persists between runs), else the temp dir.
pub fn resolve_cache_dir(over: Option<&std::ffi::OsStr>) -> std::path::PathBuf {
    match over {
        Some(dir) if !dir.is_empty() => dir.into(),
        _ => std::env::temp_dir().join("wasm2rs-testcache"),
    }
}

/// The shared cache directory, created, and pruned once per process (see [`prune`]).
///
/// Shared by every checkout on purpose: what it holds directly (`gen-*`, `ex-*`, the
/// `wasm2rs-wasi` rlib copies) is content-addressed and published by atomic rename, so concurrent
/// runs from different worktrees only ever agree on a file's bytes, and each reuses the others'
/// compiles. What a checkout writes in place lives in [`checkout_dir`] instead.
fn cache_dir() -> std::path::PathBuf {
    static PRUNED: OnceLock<()> = OnceLock::new();
    let dir = resolve_cache_dir(std::env::var_os("WASM2RS_TESTCACHE_DIR").as_deref());
    std::fs::create_dir_all(&dir).expect("create cache dir");
    PRUNED.get_or_init(|| {
        // Concurrent test processes would each scan 45k entries for nothing: the stamp's mtime
        // says when somebody last pruned.
        let stamp = dir.join(".pruned");
        let recent = std::fs::metadata(&stamp)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age < std::time::Duration::from_secs(10 * 60));
        if !recent {
            std::fs::write(&stamp, b"").ok();
            prune::prune(
                &dir,
                std::time::SystemTime::now(),
                &prune::Policy::from_env(),
            );
        }
    });
    dir
}

/// Points `cmd` (a `cargo run --example wasip2_*` of the tests) at the shared cache: the
/// `wasm2rs-wasi` build of the example is made once per checkout instead of once per run, and the
/// program it builds is reused when its inputs are the same.
pub fn share_example_build(cmd: &mut Command) -> &mut Command {
    cmd.env(
        "WASM2RS_EXAMPLE_WASI_TARGET",
        checkout_dir().join("example-wasi-target"),
    )
    .env("WASM2RS_EXAMPLE_CACHE_DIR", cache_dir())
}

/// The directory of the cache root that belongs to the checkout at `manifest_dir`: worktrees with
/// different host sources must not overwrite each other's `wasm2rs-wasi` build, nor write the same
/// scratch file. [`prune`] recognises it by the `checkout-` prefix.
pub fn checkout_dir_in(
    root: &std::path::Path,
    manifest_dir: &std::path::Path,
) -> std::path::PathBuf {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    manifest_dir.hash(&mut h);
    root.join(format!("checkout-{:016x}", h.finish()))
}

/// This checkout's own directory in the cache (see [`checkout_dir_in`]), created, and marked as
/// used once per process so that [`prune`] does not take a checkout that is being worked in for an
/// abandoned one.
fn checkout_dir() -> std::path::PathBuf {
    static USED: OnceLock<()> = OnceLock::new();
    let dir = checkout_dir_in(
        &cache_dir(),
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
    );
    std::fs::create_dir_all(&dir).expect("create checkout dir");
    USED.get_or_init(|| std::fs::write(dir.join(".used"), b"").expect("mark checkout dir used"));
    dir
}

/// Makes the names of scratch directories unique within the process (the pid covers processes).
static SCRATCH_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A new, empty scratch directory for `label`, unique per call and process. Use this rather than
/// `$TMPDIR/<fixed name>`, which every checkout and process running the same test shares and
/// overwrites. Named like the build working directories, so [`prune`] removes what a failed test
/// left behind after a few hours.
pub fn scratch_dir(label: &str) -> std::path::PathBuf {
    let uniq = SCRATCH_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = cache_dir().join(format!("work-{label}-{}-{uniq}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn build_with(name: &str, program: &str, wasi: Option<&WasiLib>) -> std::path::PathBuf {
    let cache_dir = cache_dir();
    let exe = if cfg!(windows) { ".exe" } else { "" };
    let bin = cache_dir.join(format!("{}{exe}", cache_key(program, wasi)));
    if bin.exists() {
        prune::touch(&bin);
        return bin;
    }

    // Cache miss: compile in a per-call working dir so parallel rustc processes
    // never share codegen-unit temp objects, then atomically publish the binary
    // into the shared cache. Concurrent misses of the same key both compile and
    // rename; the last rename wins and every waiter observes a complete binary.
    let uniq = SCRATCH_COUNTER.fetch_add(1, Ordering::Relaxed);
    let work = cache_dir.join(format!("work-{name}-{}-{uniq}", std::process::id()));
    std::fs::create_dir_all(&work).expect("create work dir");
    let src = work.join("gen.rs");
    let staged = work.join(format!("gen{exe}"));
    std::fs::write(&src, program).expect("write generated source");

    let mut cmd = Command::new("rustc");
    cmd.current_dir(&work).arg(&src);
    cmd.args(RUSTC_FLAGS);
    if let Some(lib) = wasi {
        let mut extern_arg = std::ffi::OsString::from("wasm2rs_wasi=");
        extern_arg.push(&lib.rlib);
        cmd.arg("--extern").arg(extern_arg);
        if let Some(rmeta) = &lib.rmeta {
            let mut meta_arg = std::ffi::OsString::from("wasm2rs_wasi=");
            meta_arg.push(rmeta);
            cmd.arg("--extern").arg(meta_arg);
        }
        for dir in &lib.deps {
            let mut deps_arg = std::ffi::OsString::from("dependency=");
            deps_arg.push(dir);
            cmd.arg("-L").arg(deps_arg);
        }
    }
    cmd.arg("-o").arg(&staged);
    let out = cmd.output().expect("run rustc");
    assert!(
        out.status.success(),
        "generated code failed to compile:\n{}\n--- stderr ---\n{}\n--- command ---\n{cmd:?}\n--- deps ---\n{}",
        &program[..program.len().min(400)],
        String::from_utf8_lossy(&out.stderr),
        wasi.map(|lib| format!("{:?}", lib.deps))
            .unwrap_or_default(),
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

/// Transpile an already-assembled wasm module (e.g. one built by an external
/// toolchain), run `main_body`, and assert it exits 0.
pub fn compile_run_wasm(name: &str, wasm: &[u8], main_body: &str) {
    let generated = wasm2rs::transpile(wasm).expect("transpile ok");
    let program = format!("{generated}\nfn main() {{\n{main_body}\n}}\n");
    let bin = build(name, &program);
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
