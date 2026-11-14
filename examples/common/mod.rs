//! Shared by the `wasip2_*` examples: transpiling a component, and building a generated program against `wasm2rs-wasi`.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;

use wasm2rs::{ComponentOptions, transpile_component_with_exports};

/// Transpiles `component` with [`transpile_component_with_exports`] and checks that it lifts
/// `{interface}#{func}` (a missing export is reported here, before the build; a transpile
/// error takes precedence over a missing export). A component made with a preview1 adapter
/// qualifies: `transpile_component` links its modules and the lift is found all the same. The interface
/// matches at any patch version (`wasi:cli/run@0.2.3`, as an adapter exports it, is `@0.2.0`'s
/// too). Returns the code and the interface as the component names it. The error is the message to print.
pub fn transpile_exporting(
    component: &[u8],
    interface: &str,
    func: &str,
    typed_traps: bool,
) -> Result<(String, String), String> {
    let mut options = ComponentOptions::default();
    options.transpile.typed_traps = typed_traps;
    let (code, exports) =
        transpile_component_with_exports(component, &options).map_err(|e| e.to_string())?;
    let Some(found) = exports
        .iter()
        .find(|e| e.func.name == func && same_minor_version(&e.func.interface, interface))
    else {
        return Err(format!(
            "the component does not export `{interface}#{func}`"
        ));
    };
    Ok((code, found.func.interface.clone()))
}

/// Whether `a` and `b` name the same interface at versions that differ in the patch number at most.
fn same_minor_version(a: &str, b: &str) -> bool {
    fn minor(name: &str) -> Option<(&str, &str)> {
        let (base, version) = name.split_once('@')?;
        let (minor, _patch) = version.rsplit_once('.')?;
        Some((base, minor))
    }
    a == b || matches!((minor(a), minor(b)), (Some(x), Some(y)) if x == y)
}

/// Where the `wasm2rs-wasi` library is built: `$WASM2RS_EXAMPLE_WASI_TARGET`, which several runs
/// may share, or else `dir`.
fn wasi_target(dir: &Path) -> PathBuf {
    std::env::var_os("WASM2RS_EXAMPLE_WASI_TARGET")
        .map(PathBuf::from)
        .unwrap_or_else(|| dir.join("wasi-target"))
}

/// Keeps concurrent runs that share a target directory from using the library while another
/// one's `cargo` links it again (it unlinks and re-creates the artifact files on every
/// invocation, even when nothing is stale). `cargo` runs under the exclusive lock; the lock is
/// then held shared while the files are read and `rustc` links against them.
struct SharedTargetLock(Option<std::fs::File>);

impl SharedTargetLock {
    fn exclusive(target: &Path) -> Self {
        if std::env::var_os("WASM2RS_EXAMPLE_WASI_TARGET").is_none() {
            return Self(None);
        }
        let mut name = target.as_os_str().to_owned();
        name.push(".lock");
        let lock = std::fs::File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(name)
            .expect("open the target lock");
        lock.lock().expect("lock the target");
        Self(Some(lock))
    }

    fn downgrade(&self) {
        if let Some(lock) = &self.0 {
            // unlock first: asking for a shared lock while holding the exclusive one is unspecified
            // (it may deadlock on Windows); anyone who gets in between only runs cargo again
            lock.unlock().expect("unlock the target");
            lock.lock_shared().expect("share the target lock");
        }
    }
}

/// Builds the `wasm2rs-wasi` library with `cargo` into `target` (it depends on `getrandom`, which
/// a bare `rustc` call cannot resolve): the rlib and the directory of its dependencies' rlibs.
fn build_wasi_rlib(target: &Path) -> (PathBuf, Option<PathBuf>, Vec<PathBuf>) {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let out = Command::new(cargo)
        .args([
            "build",
            "--release",
            "-p",
            "wasm2rs-wasi",
            "--message-format=json-render-diagnostics",
            "--manifest-path",
        ])
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
        .arg("--target-dir")
        .arg(target)
        .stderr(std::process::Stdio::inherit())
        .output()
        .expect("run cargo");
    assert!(out.status.success(), "wasm2rs-wasi failed to build");
    // cargo says where it put the rlibs (it depends on the version): the library, and the
    // directories to search for its dependencies.
    let mut lib = None;
    let mut rmeta = None;
    let mut dirs: Vec<PathBuf> = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
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
        let is_lib = line.contains("\"name\":\"wasm2rs_wasi\"");
        if is_lib {
            // newer cargo leaves only a metadata stub in the rlib: the .rmeta must be given too
            rmeta = list
                .split('"')
                .find(|f| f.ends_with(".rmeta"))
                .map(PathBuf::from);
        }
        for rlib in list.split('"').filter(|f| f.ends_with(".rlib")) {
            let rlib = PathBuf::from(rlib);
            if is_lib {
                lib = Some(rlib.clone());
            }
            if let Some(dir) = rlib.parent().filter(|d| !dirs.iter().any(|x| x == d)) {
                dirs.push(dir.to_path_buf());
            }
        }
    }
    (
        lib.expect("cargo reported no wasm2rs-wasi rlib"),
        rmeta,
        dirs,
    )
}

/// The `rustc --version` line, which a cached binary is only valid for.
fn rustc_version() -> String {
    let out = Command::new("rustc")
        .arg("--version")
        .output()
        .expect("query rustc version");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Content-addressed name of the binary built from `source`: everything that decides what the
/// binary is (the source, the optimization level, the toolchain and the bytes of the linked
/// `wasm2rs-wasi`) goes into the key. `DefaultHasher` has fixed keys, so the name is stable
/// across processes.
fn cache_name(source: &str, opt_level: &str, rlib: &Path, rmeta: Option<&Path>) -> String {
    let mut h = DefaultHasher::new();
    source.hash(&mut h);
    opt_level.hash(&mut h);
    rustc_version().hash(&mut h);
    std::fs::read(rlib)
        .expect("read wasm2rs-wasi rlib")
        .hash(&mut h);
    if let Some(rmeta) = rmeta {
        std::fs::read(rmeta)
            .expect("read wasm2rs-wasi rmeta")
            .hash(&mut h);
    }
    format!("ex-{:016x}", h.finish())
}

/// Writes `source` to `dir/program.rs` and builds it with `rustc -C opt-level=1` (or the level in
/// `$WASM2RS_EXAMPLE_OPT_LEVEL`: LLVM does not finish the 125 MB JS component at 1 in 80 minutes,
/// at 0 it takes 73 s), linked against the `wasm2rs-wasi` library (built with `cargo` into `dir` first). Returns the binary, or
/// `None` when `rustc` failed (its errors are on stderr).
///
/// With `$WASM2RS_EXAMPLE_CACHE_DIR` set, the binary is kept there under a content-addressed name
/// and reused by later runs of the same program; unset, every run builds its own.
pub fn build_program(dir: &Path, source: &str) -> Option<PathBuf> {
    let src = dir.join("program.rs");
    let bin = dir.join("program");
    std::fs::write(&src, source).expect("write source");
    let target = wasi_target(dir);
    let lock = SharedTargetLock::exclusive(&target);
    let (rlib, rmeta, deps) = build_wasi_rlib(&target);
    let opt_level = std::env::var("WASM2RS_EXAMPLE_OPT_LEVEL").unwrap_or_else(|_| "1".into());
    let cached = std::env::var_os("WASM2RS_EXAMPLE_CACHE_DIR").map(|cache| {
        let cache = PathBuf::from(cache);
        std::fs::create_dir_all(&cache).expect("create example cache dir");
        cache.join(cache_name(source, &opt_level, &rlib, rmeta.as_deref()))
    });
    if let Some(hit) = cached.as_ref().filter(|p| p.exists()) {
        // a use keeps the entry from being pruned as stale (opened read-only: a binary that is
        // open for writing cannot be `exec`ed, and a concurrent run may be doing that)
        if let Ok(file) = std::fs::File::open(hit) {
            let _ = file.set_modified(std::time::SystemTime::now());
        }
        return Some(hit.clone());
    }
    lock.downgrade();
    let status = Command::new("rustc")
        .args(["--edition", "2024", "-A", "warnings"])
        .arg("-C")
        .arg(format!("opt-level={opt_level}"))
        .arg("--extern")
        .arg(format!("wasm2rs_wasi={}", rlib.display()))
        .args(rmeta.iter().flat_map(|m| {
            [
                "--extern".to_string(),
                format!("wasm2rs_wasi={}", m.display()),
            ]
        }))
        .args(
            deps.iter()
                .flat_map(|d| ["-L".to_string(), format!("dependency={}", d.display())]),
        )
        .arg(&src)
        .arg("-o")
        .arg(&bin)
        .status()
        .expect("run rustc");
    if !status.success() {
        return None;
    }
    let Some(cached) = cached else {
        return Some(bin);
    };
    // publish atomically: concurrent builds of one program both rename, and a reader never sees
    // a partial file
    let staged = cached.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::copy(&bin, &staged).expect("stage the program into the cache");
    std::fs::rename(&staged, &cached).expect("publish the program into the cache");
    Some(cached)
}
