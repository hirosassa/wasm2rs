//! Shared by the `wasip2_*` examples: transpiling a component, and building a generated program against `wasm2rs-wasi`.

use std::path::{Path, PathBuf};
use std::process::Command;

use wasm2rs::{ComponentOptions, transpile_component_with_exports};

/// Transpiles `component` with [`transpile_component_with_exports`] and checks that it lifts
/// `{interface}#{func}` directly (a missing export, or an adapter in between, is reported here,
/// before the build; a transpile error takes precedence over a missing export). The interface
/// matches at any patch version (`wasi:cli/run@0.2.3`, as an adapter exports it, is `@0.2.0`'s
/// too). Returns the code and the interface as the component names it. The error is the message to print.
pub fn transpile_exporting(
    component: &[u8],
    interface: &str,
    func: &str,
) -> Result<(String, String), String> {
    let (code, exports) = transpile_component_with_exports(component, &ComponentOptions::default())
        .map_err(|e| e.to_string())?;
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

/// Builds the `wasm2rs-wasi` library with `cargo` into `dir` (it depends on `getrandom`, which a
/// bare `rustc` call cannot resolve): the rlib and the directory of its dependencies' rlibs.
fn build_wasi_rlib(dir: &Path) -> (PathBuf, Option<PathBuf>, Vec<PathBuf>) {
    let target = dir.join("wasi-target");
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
        .arg(&target)
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

/// Writes `source` to `dir/program.rs` and builds it with `rustc -C opt-level=1`, linked against
/// the `wasm2rs-wasi` library (built with `cargo` into `dir` first). Returns the binary, or
/// `None` when `rustc` failed (its errors are on stderr).
pub fn build_program(dir: &Path, source: &str) -> Option<PathBuf> {
    let src = dir.join("program.rs");
    let bin = dir.join("program");
    std::fs::write(&src, source).expect("write source");
    let (rlib, rmeta, deps) = build_wasi_rlib(dir);
    let status = Command::new("rustc")
        .args(["--edition", "2024", "-C", "opt-level=1", "-A", "warnings"])
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
    status.success().then_some(bin)
}
