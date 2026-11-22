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
//! The helpers above write one file. [`run_to_out_dir`] takes a [`Transpiler`] instead, so a
//! build script can also split the output and write data side files into `$OUT_DIR/{name}/`;
//! include it with `include!(concat!(env!("OUT_DIR"), "/{name}/lib.rs"))`.
//!
//! A WebAssembly *component* goes through [`transpile_component_to_out_dir`], which names the
//! output after the input (`assets/run.wasm` becomes `$OUT_DIR/run.rs`), and the
//! `wasm2rs_macros::include_component!("run")` macro includes it.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::{
    ComponentOptions, ComponentTranspiler, FileContent, OutputFile, TranspileError,
    TranspileOptions, Transpiler, transpile_component, transpile_with_options,
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
    let out_dir = cargo_out_dir(wasm_path.as_ref())?;
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
    transpile_file(dir.as_ref().join(out_name), wasm_path.as_ref(), |wasm| {
        transpile_with_options(wasm, opts)
    })
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
    let out_dir = cargo_out_dir(component_path.as_ref())?;
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
    let stem = component_stem(component_path);
    let mut name = stem.to_os_string();
    name.push(".rs");
    transpile_file(dir.as_ref().join(name), component_path, |component| {
        transpile_component(component, options)
    })
}

/// Transpile `wasm_path` with `transpiler` into the directory `$OUT_DIR/{name}` and return the
/// path of its `lib.rs`.
///
/// Unlike [`transpile_to_out_dir`], this can write several files: the split chunks
/// (`funcs_{n}.rs`) and the data side files (`data_{n}.bin`) of a [`Transpiler`] configured with
/// [`with_split`](Transpiler::with_split) / [`with_data_files`](Transpiler::with_data_files)
/// all go next to `lib.rs`. Without either, `lib.rs` alone is written, byte-identical to
/// [`Transpiler::transpile`]. Include the result with
/// `include!(concat!(env!("OUT_DIR"), "/{name}/lib.rs"))`: the `mod funcs_{n};` and
/// `include_bytes!` lines resolve next to the included file.
///
/// Also prints `cargo:rerun-if-changed` for the wasm file. Files an earlier run left in the
/// directory are not removed; they are not referenced by the new `lib.rs`. The directory is
/// created before transpiling, so a failure can leave it empty, or with the files written so far.
pub fn run_to_out_dir(
    transpiler: &Transpiler,
    wasm_path: impl AsRef<Path>,
    name: &str,
) -> Result<PathBuf, BuildError> {
    let out_dir = cargo_out_dir(wasm_path.as_ref())?;
    run_to_dir(transpiler, out_dir, wasm_path, name)
}

/// [`run_to_out_dir`] with the output directory given explicitly (the files go to
/// `{dir}/{name}`, which is created), and without the Cargo output.
pub fn run_to_dir(
    transpiler: &Transpiler,
    dir: impl AsRef<Path>,
    wasm_path: impl AsRef<Path>,
    name: &str,
) -> Result<PathBuf, BuildError> {
    let wasm = read_input(wasm_path.as_ref())?;
    write_files(dir.as_ref().join(name), |sink| transpiler.run(&wasm, sink))
}

/// Transpile the component at `component_path` with `transpiler` into the directory
/// `$OUT_DIR/{stem}`, where `{stem}` is the file name without its extension, and return the path
/// of its `lib.rs`. The component counterpart of [`run_to_out_dir`]: split chunks and data side
/// files go next to `lib.rs`, which holds the glue and `Exports` and is written last.
///
/// Include it with `include!(concat!(env!("OUT_DIR"), "/{stem}/lib.rs"))`;
/// `wasm2rs_macros::include_component!` reads the single-file `{stem}.rs` of
/// [`transpile_component_to_out_dir`] only.
pub fn run_component_to_out_dir(
    transpiler: &ComponentTranspiler,
    component_path: impl AsRef<Path>,
) -> Result<PathBuf, BuildError> {
    let out_dir = cargo_out_dir(component_path.as_ref())?;
    run_component_to_dir(transpiler, out_dir, component_path)
}

/// [`run_component_to_out_dir`] with the output directory given explicitly, and without the
/// Cargo output.
pub fn run_component_to_dir(
    transpiler: &ComponentTranspiler,
    dir: impl AsRef<Path>,
    component_path: impl AsRef<Path>,
) -> Result<PathBuf, BuildError> {
    let component_path = component_path.as_ref();
    let stem = component_stem(component_path);
    let component = read_input(component_path)?;
    write_files(dir.as_ref().join(stem), |sink| {
        transpiler.run(&component, sink).map(|_exports| ())
    })
}

/// `$OUT_DIR`, after telling Cargo to rerun the build script when `input` changes.
fn cargo_out_dir(input: &Path) -> Result<std::ffi::OsString, BuildError> {
    let out_dir = std::env::var_os("OUT_DIR").ok_or(BuildError::MissingOutDir)?;
    println!("cargo:rerun-if-changed={}", input.display());
    Ok(out_dir)
}

/// Read `input`, transpile it with `transpile` and write the result to `out`, which is returned.
/// Nothing is written when reading or transpiling fails.
fn transpile_file(
    out: PathBuf,
    input: &Path,
    transpile: impl FnOnce(&[u8]) -> Result<String, TranspileError>,
) -> Result<PathBuf, BuildError> {
    let bytes = read_input(input)?;
    let code = transpile(&bytes)?;
    write_output(&out, code.as_bytes())?;
    Ok(out)
}

/// The component's file name without its extension, which names its output.
fn component_stem(path: &Path) -> &std::ffi::OsStr {
    path.file_stem().unwrap_or(path.as_os_str())
}

fn write_output(path: &Path, bytes: &[u8]) -> Result<(), BuildError> {
    std::fs::write(path, bytes).map_err(|source| BuildError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn read_input(input: &Path) -> Result<Vec<u8>, BuildError> {
    std::fs::read(input).map_err(|source| BuildError::Io {
        path: input.to_path_buf(),
        source,
    })
}

/// Create `out_dir` and run `emit` with a sink that writes each file into it. Returns the path
/// of `lib.rs`. A write error is kept as [`BuildError::Io`] (the sink stops the run with an
/// `Internal` placeholder that is replaced by it).
fn write_files(
    out_dir: PathBuf,
    emit: impl FnOnce(
        &mut dyn FnMut(OutputFile) -> Result<(), TranspileError>,
    ) -> Result<(), TranspileError>,
) -> Result<PathBuf, BuildError> {
    std::fs::create_dir_all(&out_dir).map_err(|source| BuildError::Io {
        path: out_dir.clone(),
        source,
    })?;
    let mut failed = None;
    let result = emit(&mut |file: OutputFile| {
        let bytes = match &file.content {
            FileContent::Text(text) => text.as_bytes(),
            FileContent::Bytes(bytes) => bytes,
        };
        write_output(&out_dir.join(&file.name), bytes).map_err(|e| {
            failed = Some(e);
            TranspileError::Internal("write failed".into())
        })
    });
    if let Some(e) = failed {
        return Err(e);
    }
    result?;
    Ok(out_dir.join("lib.rs"))
}
