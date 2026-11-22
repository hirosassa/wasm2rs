//! [`Transpiler`]: one entry point for transpiling a core module, configured by chaining.
//!
//! The crate-root `transpile*` functions are a product of axes (split, data side files,
//! [`TranspileOptions`]) that doubles with each new axis; the builder keeps them as state and
//! the functions wrap it.

use crate::{
    OutputFile, SourcePieces, SplitOptions, TranspileError, TranspileOptions, codegen,
    transpile_impl, transpile_with_host_imports,
};

/// Transpiles a core wasm module, configured by chaining.
///
/// Start from [`Transpiler::new`] (or [`Default`], the safe, portable output of
/// [`transpile`](crate::transpile)), add the axes you need with the `with_*` methods, then pick
/// the shape of the output with a terminal method:
///
/// | terminal | output |
/// |---|---|
/// | [`transpile`](Transpiler::transpile) | one source string |
/// | [`transpile_pieces`](Transpiler::transpile_pieces) | one source in three pieces, no join copy |
/// | [`run`](Transpiler::run) | files streamed to a sink, split per [`with_split`](Transpiler::with_split) |
///
/// ```
/// use wasm2rs::{SplitOptions, TranspileOptions, Transpiler};
///
/// # fn main() -> Result<(), wasm2rs::TranspileError> {
/// # let wasm = wat::parse_str("(module (func (export \"f\")))").unwrap();
/// let split = SplitOptions { funcs_per_file: 500, max_bytes_per_file: 0 };
/// Transpiler::new(TranspileOptions::default().with_unsafe_memory(true))
///     .with_split(split)
///     .with_data_files(true)
///     .run(&wasm, |file| {
///         // write `file.name` (relative to the crate's `src/`) with `file.content`
///         Ok(())
///     })?;
/// # Ok(())
/// # }
/// ```
///
/// `#[non_exhaustive]` like [`TranspileOptions`], so a new axis is not a breaking change.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct Transpiler {
    options: TranspileOptions,
    split: SplitOptions,
    data_files: bool,
}

impl Transpiler {
    /// A transpiler for `options`, writing a single file with every data segment inline.
    pub fn new(options: TranspileOptions) -> Self {
        Self {
            options,
            ..Self::default()
        }
    }

    /// Split the output across files (see [`SplitOptions`]). Only [`run`](Self::run) can emit
    /// more than one file. Default: [`SplitOptions::single_file`].
    #[must_use]
    pub fn with_split(mut self, split: SplitOptions) -> Self {
        self.split = split;
        self
    }

    /// Move the bytes of each data segment of 4 KiB or more into a `data_{n}.bin` side file
    /// next to `lib.rs`, pulled in with `include_bytes!`, so `rustc` reads them raw instead of
    /// parsing a huge literal. [`run`](Self::run) may then hand the sink
    /// [`FileContent::Bytes`](crate::FileContent::Bytes). Default: `false`.
    #[must_use]
    pub fn with_data_files(mut self, data_files: bool) -> Self {
        self.data_files = data_files;
        self
    }

    /// Transpile `wasm` into a single Rust source string.
    ///
    /// # Errors
    ///
    /// [`TranspileError::InvalidOptions`] if a split or [`with_data_files`](Self::with_data_files)
    /// was configured (use [`run`](Self::run)), plus every error of [`run`](Self::run).
    pub fn transpile(&self, wasm: &[u8]) -> Result<String, TranspileError> {
        self.require_single_file()?;
        transpile_with_host_imports(wasm, &self.options).map(|(code, _)| code)
    }

    /// Transpile `wasm` into a single Rust source, returned in three consecutive pieces.
    ///
    /// The concatenation of the pieces is byte-identical to [`transpile`](Self::transpile);
    /// writing them one after another (see [`SourcePieces::write_to`]) avoids copying the bulk of
    /// a large output into a second contiguous buffer.
    ///
    /// # Errors
    ///
    /// As [`transpile`](Self::transpile).
    pub fn transpile_pieces(&self, wasm: &[u8]) -> Result<SourcePieces, TranspileError> {
        self.require_single_file()?;
        transpile_impl(wasm, &self.options, codegen::generate_module_pieces)
    }

    /// Transpile `wasm`, handing each [`OutputFile`] to `sink` as it completes so the peak
    /// memory stays around one file.
    ///
    /// Without a split exactly one `lib.rs` is emitted, identical to
    /// [`transpile`](Self::transpile); with one, a `lib.rs` root plus `funcs_{n}.rs` chunks.
    ///
    /// # Errors
    ///
    /// A [`TranspileError`] for a module that cannot be transpiled under the options, plus
    /// whatever `sink` returns (which stops the run).
    pub fn run<F>(&self, wasm: &[u8], mut sink: F) -> Result<(), TranspileError>
    where
        F: FnMut(OutputFile) -> Result<(), TranspileError>,
    {
        transpile_impl(wasm, &self.options, |parts| {
            codegen::generate_module_split(
                parts,
                self.split.funcs_per_file,
                self.split.max_bytes_per_file,
                self.data_files,
                &mut sink,
            )
        })
    }

    fn require_single_file(&self) -> Result<(), TranspileError> {
        require_single_file(&self.split, self.data_files, "Transpiler")
    }
}

/// Reject the axes a single-source terminal of `builder` cannot honour: a string cannot carry
/// several files.
pub(crate) fn require_single_file(
    split: &SplitOptions,
    data_files: bool,
    builder: &str,
) -> Result<(), TranspileError> {
    if split.funcs_per_file != 0 || split.max_bytes_per_file != 0 {
        return Err(TranspileError::InvalidOptions(format!(
            "a split output has several files; use {builder}::run"
        )));
    }
    if data_files {
        return Err(TranspileError::InvalidOptions(format!(
            "data side files need {builder}::run"
        )));
    }
    Ok(())
}
