//! Helpers for using wasm2rs from a Cargo build script.
//!
//! ```ignore
//! // build.rs
//! fn main() {
//!     wasm2rs::build::transpile_to_out_dir("assets/foo.wasm", "foo.rs").unwrap();
//! }
//! ```
//!
//! ```ignore
//! // src/lib.rs
//! mod foo {
//!     include!(concat!(env!("OUT_DIR"), "/foo.rs"));
//! }
//! ```
//!
//! A WebAssembly *component* goes through [`transpile_component_to_out_dir`], which names the
//! output after the input (`assets/run.wasm` becomes `$OUT_DIR/run.rs`), and the
//! `wasm2rs_macros::include_component!("run")` macro includes it.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::{
    ComponentOptions, TranspileError, TranspileOptions, transpile_component, transpile_with_options,
};

/// An error from the build helpers.
#[derive(Debug)]
pub enum BuildError {
    /// `OUT_DIR` is not set, so the function was not called from a build script.
    MissingOutDir,
    /// Reading the wasm file or writing the output failed.
    Io {
        /// The file that could not be read or written.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
    /// The wasm module could not be transpiled.
    Transpile(TranspileError),
}

impl fmt::Display for BuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingOutDir => write!(f, "OUT_DIR is not set (not run from a build script)"),
            Self::Io { path, source } => write!(f, "{}: {source}", path.display()),
            Self::Transpile(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for BuildError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::MissingOutDir => None,
            Self::Io { source, .. } => Some(source),
            Self::Transpile(e) => Some(e),
        }
    }
}

impl From<TranspileError> for BuildError {
    fn from(e: TranspileError) -> Self {
        Self::Transpile(e)
    }
}

/// Transpile `wasm_path` into `$OUT_DIR/{out_name}` and return that path.
///
/// Also prints `cargo:rerun-if-changed` for the wasm file, so Cargo reruns the
/// build script when it changes. Call this from `build.rs`.
pub fn transpile_to_out_dir(
    wasm_path: impl AsRef<Path>,
    out_name: &str,
) -> Result<PathBuf, BuildError> {
    transpile_to_out_dir_with_options(wasm_path, out_name, &TranspileOptions::default())
}

/// Like [`transpile_to_out_dir`], with explicit [`TranspileOptions`] (for
/// example `named_exports`).
pub fn transpile_to_out_dir_with_options(
    wasm_path: impl AsRef<Path>,
    out_name: &str,
    opts: &TranspileOptions,
) -> Result<PathBuf, BuildError> {
    let out_dir = std::env::var_os("OUT_DIR").ok_or(BuildError::MissingOutDir)?;
    println!("cargo:rerun-if-changed={}", wasm_path.as_ref().display());
    transpile_to_dir_with_options(out_dir, wasm_path, out_name, opts)
}

/// Transpile `wasm_path` into `{dir}/{out_name}` and return that path. This is
/// [`transpile_to_out_dir`] with the output directory given explicitly, and
/// without the Cargo output.
pub fn transpile_to_dir(
    dir: impl AsRef<Path>,
    wasm_path: impl AsRef<Path>,
    out_name: &str,
) -> Result<PathBuf, BuildError> {
    transpile_to_dir_with_options(dir, wasm_path, out_name, &TranspileOptions::default())
}

/// Like [`transpile_to_dir`], with explicit [`TranspileOptions`].
pub fn transpile_to_dir_with_options(
    dir: impl AsRef<Path>,
    wasm_path: impl AsRef<Path>,
    out_name: &str,
    opts: &TranspileOptions,
) -> Result<PathBuf, BuildError> {
    let wasm_path = wasm_path.as_ref();
    let wasm = std::fs::read(wasm_path).map_err(|source| BuildError::Io {
        path: wasm_path.to_path_buf(),
        source,
    })?;
    let code = transpile_with_options(&wasm, opts)?;
    let out = dir.as_ref().join(out_name);
    std::fs::write(&out, code).map_err(|source| BuildError::Io {
        path: out.clone(),
        source,
    })?;
    Ok(out)
}

/// Transpile the component at `component_path` into `$OUT_DIR/{stem}.rs`, where `{stem}` is the
/// file name without its extension, and return that path. See [`transpile_component`] for what
/// is generated.
///
/// Also prints `cargo:rerun-if-changed` for the input. Call this from `build.rs`, and include the
/// result with `wasm2rs_macros::include_component!("{stem}")`.
pub fn transpile_component_to_out_dir(
    component_path: impl AsRef<Path>,
    options: &ComponentOptions,
) -> Result<PathBuf, BuildError> {
    let out_dir = std::env::var_os("OUT_DIR").ok_or(BuildError::MissingOutDir)?;
    println!(
        "cargo:rerun-if-changed={}",
        component_path.as_ref().display()
    );
    transpile_component_to_dir(out_dir, component_path, options)
}

/// [`transpile_component_to_out_dir`] with the output directory given explicitly, and without
/// the Cargo output.
pub fn transpile_component_to_dir(
    dir: impl AsRef<Path>,
    component_path: impl AsRef<Path>,
    options: &ComponentOptions,
) -> Result<PathBuf, BuildError> {
    let component_path = component_path.as_ref();
    let component = std::fs::read(component_path).map_err(|source| BuildError::Io {
        path: component_path.to_path_buf(),
        source,
    })?;
    let code = transpile_component(&component, options)?;
    let stem = component_path
        .file_stem()
        .unwrap_or(component_path.as_os_str());
    let mut name = stem.to_os_string();
    name.push(".rs");
    let out = dir.as_ref().join(name);
    std::fs::write(&out, code).map_err(|source| BuildError::Io {
        path: out.clone(),
        source,
    })?;
    Ok(out)
}
