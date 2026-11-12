//! wasm2rs: convert a WebAssembly binary into standalone Rust source code.
//!
//! The generated Rust preserves WebAssembly semantics (wrapping arithmetic,
//! bounds-checked linear memory, traps) with no runtime dependency. Supported:
//! the MVP value types (`i32`/`i64`/`f32`/`f64`), SIMD and relaxed SIMD,
//! atomics and shared memory, multi-memory, tail calls, exception handling,
//! GC, stack-switching continuations, and native WASI preview1 /
//! wasi-threads. Entry points: [`transpile`], [`transpile_with_options`],
//! [`transpile_split`] and [`transpile_split_with_options`]; a component goes in through
//! [`transpile_component`].

pub mod build;
mod codegen;
mod component;
mod dump;
mod extract;
mod parse;
mod wit;
mod wit_abi;

pub use component::{ComponentOptions, transpile_component, transpile_component_with_exports};
pub use dump::{ModuleSummary, dump};
pub use extract::extract_core_modules;
pub use wit::{
    WitExport, WitFunc, WitNaming, WitOptions, WitRecord, WitResource, WitType, wit_exports,
    wit_functions, wit_host_glue, wit_host_glue_with_options, wit_host_trait,
    wit_host_trait_with_options,
};

/// An error that can occur while transpiling a wasm module.
///
/// The enum is `#[non_exhaustive]`: match the kinds you care about and keep a
/// wildcard arm. [`TranspileError::root`] peels the per-function context
/// ([`InFunction`](TranspileError::InFunction)) off to reach the underlying kind.
#[derive(Debug)]
#[non_exhaustive]
pub enum TranspileError {
    /// The wasm binary could not be parsed, or failed validation.
    Parse(wasmparser::BinaryReaderError),
    /// The module used a feature that is not supported yet.
    Unsupported(String),
    /// The options passed to the transpiler contradict each other, or the
    /// module (for example `entry_exports` naming a missing export).
    InvalidOptions(String),
    /// The input is not what the entry point expects (for example a component
    /// where a core module is required, or a component with no usable core
    /// module).
    InvalidInput(String),
    /// A transpiler invariant was violated: a bug in wasm2rs, not in the input.
    Internal(String),
    /// `source` happened while transpiling the function at `index` (its index
    /// in the module's function index space, imports included).
    InFunction {
        index: usize,
        source: Box<TranspileError>,
    },
    /// The operand stack was empty when a value was required, indicating a
    /// malformed module (or a bug in the transpiler).
    StackUnderflow,
}

impl std::fmt::Display for TranspileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse(e) => write!(f, "failed to parse wasm: {e}"),
            Self::Unsupported(what) => write!(f, "unsupported: {what}"),
            Self::InvalidOptions(what) => write!(f, "invalid options: {what}"),
            Self::InvalidInput(what) => write!(f, "invalid input: {what}"),
            Self::Internal(what) => write!(f, "internal error: {what}"),
            Self::InFunction { index, source } => write!(f, "in function {index}: {source}"),
            Self::StackUnderflow => write!(f, "operand stack underflow"),
        }
    }
}

impl TranspileError {
    /// The underlying error, with any [`InFunction`](Self::InFunction) context removed.
    pub fn root(&self) -> &TranspileError {
        match self {
            Self::InFunction { source, .. } => source.root(),
            other => other,
        }
    }
}

impl std::error::Error for TranspileError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Parse(e) => Some(e),
            Self::InFunction { source, .. } => Some(source.as_ref()),
            _ => None,
        }
    }
}

impl From<wasmparser::BinaryReaderError> for TranspileError {
    fn from(e: wasmparser::BinaryReaderError) -> Self {
        Self::Parse(e)
    }
}

impl From<std::fmt::Error> for TranspileError {
    fn from(_: std::fmt::Error) -> Self {
        // Writing into an in-memory `String` is infallible in practice; this
        // conversion exists only to satisfy `write!`'s `Result` contract.
        Self::Internal("string formatting failed".into())
    }
}

/// One emitted Rust source file of a transpiled module.
///
/// `name` is a file name relative to the output directory (`lib.rs` is the
/// crate root; the chunk files are `funcs_0.rs`, `funcs_1.rs`, ...).
pub struct SourceFile {
    pub name: String,
    pub code: String,
}

/// A single-file output as three consecutive pieces: the text before the
/// function bodies, the bodies (the bulk), and the text after them.
pub struct SourcePieces {
    pub head: String,
    pub body: String,
    pub tail: String,
}

impl SourcePieces {
    /// Write `head`, `body` and `tail` in order.
    pub fn write_to(&self, w: &mut (impl std::io::Write + ?Sized)) -> std::io::Result<()> {
        w.write_all(self.head.as_bytes())?;
        w.write_all(self.body.as_bytes())?;
        w.write_all(self.tail.as_bytes())
    }

    /// Join the pieces into one string.
    pub fn into_string(self) -> String {
        // Only for callers that need one string: the join copies the body.
        let mut out = self.head;
        out.reserve(self.body.len() + self.tail.len());
        out.push_str(&self.body);
        out.push_str(&self.tail);
        out
    }
}

/// How to split a module's generated source across files.
///
/// A chunk file is closed once it reaches `funcs_per_file` functions *or*
/// `max_bytes_per_file` bytes, whichever comes first (`0` disables that cap).
/// The byte cap is what bounds peak memory when a module has a few very large
/// functions, where a fixed function count can still produce a huge file.
pub struct SplitOptions {
    /// The maximum number of defined functions per chunk file (`0` = no limit).
    pub funcs_per_file: usize,
    /// The approximate maximum source size per chunk file, in bytes (`0` = no
    /// limit). Enforced at function boundaries, so a single larger function is
    /// still emitted whole.
    pub max_bytes_per_file: usize,
}

impl SplitOptions {
    /// Emit the whole module as a single `lib.rs`, byte-identical to
    /// [`transpile`].
    pub fn single_file() -> Self {
        Self {
            funcs_per_file: 0,
            max_bytes_per_file: 0,
        }
    }
}

/// Code-generation options that change the emitted Rust (as opposed to how it is
/// split across files, which is [`SplitOptions`]).
///
/// The default is the safe, portable output produced by [`transpile`]; set a
/// field to opt into a faster-but-less-safe lowering.
///
/// The struct is `#[non_exhaustive]`, so a new option is not a breaking change:
/// start from [`TranspileOptions::default`] and either assign fields or chain the
/// `with_*` methods (`TranspileOptions::default().with_named_exports(true)`).
/// A struct literal, even with `..Default::default()`, only works inside this crate.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct TranspileOptions {
    /// Emit unchecked (`unsafe`) linear-memory access: every load/store and bulk
    /// `memory.*` drops its Rust slice bounds check and reads/writes through
    /// `get_unchecked`, gaining speed. **An out-of-bounds access then becomes
    /// undefined behaviour instead of a wasm trap**, so this is sound only for
    /// modules you trust to stay in bounds. Off by default. Not applied to a
    /// `shared` memory.
    pub unsafe_memory: bool,

    /// Emit `core::arch` (NEON on aarch64, SSE2 on x86_64) bodies for the
    /// integer SIMD lane helpers (add/sub/mul/neg/abs, saturating add/sub,
    /// `avgr_u`, integer compares) instead of the scalar per-lane loop, which
    /// stays as the fallback on every other target. Results are bit-identical;
    /// the generated code gains `unsafe` blocks (a same-size `transmute` and
    /// intrinsic calls whose target feature the `cfg` guarantees), so it is off
    /// by default. Float, shift and widening lane ops are unchanged.
    pub simd_intrinsics: bool,

    /// Count the activations of the module's defined functions and trap with
    /// `call stack exhausted` (a `panic!`) when a call would nest deeper than this
    /// many frames (the `wasm2c` approach). Without it, unbounded guest recursion
    /// exhausts the native stack and aborts the whole host process, which no
    /// `catch_unwind` can intercept.
    ///
    /// The counter is per thread and unwinds with the stack, so a trap or a caught
    /// exception leaves it consistent. A `return_call*` does not deepen it: the
    /// trampoline (or self-loop) keeps a tail-calling chain at one activation.
    /// Pick a limit your thread's stack can hold (the frame size depends on the
    /// module and the build profile). Continuation step functions are not counted.
    /// `Some(0)` traps on the first call. `None` (the default) emits no counter, so existing output is unchanged.
    pub call_depth_limit: Option<u32>,

    /// Split a flattened `loop { match pc { … } }` dispatch whose surviving arm
    /// count exceeds this cap into several sibling *part* functions over a shared
    /// state struct, so a pathologically large flattened function is emitted as
    /// many smaller ones the Rust backend can optimise (and codegen-parallelise)
    /// independently — without changing what it computes. The value caps the arms
    /// per part; `0` (the default) keeps each flattened function whole.
    pub split_dispatch: usize,

    /// Emit only the functions reachable from these exported functions, plus the
    /// start function and the entry points the generated code itself calls
    /// (`wasi_thread_start` and the emscripten thread start-up exports).
    /// Everything else is left out, which shortens the generated source and the
    /// Rust build; surviving functions keep their wasm-index names (`func{N}`).
    ///
    /// `None` (the default) emits every function. `Some(vec![])` keeps no export
    /// as a root. A name that is not an exported function is an error. Reading
    /// the table (`call_indirect`, `table.get/copy/init`) from reachable code
    /// keeps every element-segment function.
    pub entry_exports: Option<Vec<String>>,

    /// How many threads generate function bodies. The output is byte-identical
    /// for every value: the per-function pieces are joined in function-index
    /// order. `1` stays on the calling thread; `0` (the default) picks the
    /// available parallelism (at most 8), but only for modules large enough to
    /// repay the threads. Any other value is used as given (capped at the function
    /// count). Worker threads get a large stack of their own, since lowering
    /// recurses on control-flow nesting.
    pub jobs: usize,

    /// Also emit, for every exported (defined) function, a method or free function
    /// named after the export — `pub fn add(&mut self, a0: i32, a1: i32) -> i32`
    /// — that forwards to `func{N}`. The name is the export name made a snake_case
    /// identifier; a clash with a keyword or a generated member appends `_`, and a
    /// duplicate appends `_{function index}`. `func{N}` is kept either way. Off by
    /// default, so existing output is unchanged.
    pub named_exports: bool,

    /// Name the `Imports` trait methods after the import — `env_log` for
    /// `(import "env" "log" ...)` — instead of its function index (`import0`).
    /// The name is the module (without a `@version` suffix) and field made
    /// snake_case and joined with `_`; a clash with a keyword or another trait
    /// member appends `_`, and a duplicate appends `_{function index}`. Imports
    /// recognised as WASI preview1 stay native and get no trait method. Off by
    /// default, because renaming the methods breaks existing host implementations.
    pub named_imports: bool,

    /// Treat the module's own (defined, non-shared) memory 0 as host-owned, like an
    /// imported memory: the host lends the buffer through `Imports::memory` and
    /// `memory_mut`, so its own methods can read and write guest memory. Active
    /// data segments are written into that buffer when the instance is built, and
    /// `memory.grow` resizes it. The host must lend at least the declared minimum
    /// number of pages. Needed to implement imports that take pointers into linear
    /// memory (the lowered functions of a WASI preview2 component). No effect on a
    /// shared memory, a module that already imports its memory, or one without
    /// memory. Off by default, since it adds `Imports` methods the host must implement.
    pub import_memory: bool,

    /// Let the host call back into the guest while an import runs. Every `Imports`
    /// method then takes the `Instance` as its second parameter (`g: &mut
    /// Instance<Self>`): the host can call guest functions (`g.func{N}(..)`, for
    /// example a canonical-ABI `cabi_realloc`) and read or write guest memory
    /// (`g.memory()`), so no host-owned memory is needed. The host is lent out for
    /// the duration of the call, so a guest function reached from inside an import
    /// that calls an import again traps ("re-entrant import call"). Not combinable
    /// with `import_memory`, an imported memory, table or global, or the native
    /// `wasi::thread-spawn`. Has no effect on a module without a host function
    /// import. Off by default, since it changes every `Imports` method signature.
    pub reentrant_imports: bool,
}

impl TranspileOptions {
    /// Set [`TranspileOptions::unsafe_memory`].
    #[must_use]
    pub fn with_unsafe_memory(mut self, unsafe_memory: bool) -> Self {
        self.unsafe_memory = unsafe_memory;
        self
    }

    /// Set [`TranspileOptions::simd_intrinsics`].
    #[must_use]
    pub fn with_simd_intrinsics(mut self, simd_intrinsics: bool) -> Self {
        self.simd_intrinsics = simd_intrinsics;
        self
    }

    /// Set [`TranspileOptions::call_depth_limit`].
    #[must_use]
    pub fn with_call_depth_limit(mut self, call_depth_limit: Option<u32>) -> Self {
        self.call_depth_limit = call_depth_limit;
        self
    }

    /// Set [`TranspileOptions::split_dispatch`].
    #[must_use]
    pub fn with_split_dispatch(mut self, split_dispatch: usize) -> Self {
        self.split_dispatch = split_dispatch;
        self
    }

    /// Set [`TranspileOptions::entry_exports`].
    #[must_use]
    pub fn with_entry_exports(mut self, entry_exports: Option<Vec<String>>) -> Self {
        self.entry_exports = entry_exports;
        self
    }

    /// Set [`TranspileOptions::jobs`].
    #[must_use]
    pub fn with_jobs(mut self, jobs: usize) -> Self {
        self.jobs = jobs;
        self
    }

    /// Set [`TranspileOptions::named_exports`].
    #[must_use]
    pub fn with_named_exports(mut self, named_exports: bool) -> Self {
        self.named_exports = named_exports;
        self
    }

    /// Set [`TranspileOptions::named_imports`].
    #[must_use]
    pub fn with_named_imports(mut self, named_imports: bool) -> Self {
        self.named_imports = named_imports;
        self
    }

    /// Set [`TranspileOptions::import_memory`].
    #[must_use]
    pub fn with_import_memory(mut self, import_memory: bool) -> Self {
        self.import_memory = import_memory;
        self
    }

    /// Set [`TranspileOptions::reentrant_imports`].
    #[must_use]
    pub fn with_reentrant_imports(mut self, reentrant_imports: bool) -> Self {
        self.reentrant_imports = reentrant_imports;
        self
    }
}

/// The recommended `Cargo.toml` for a split crate, named `package_name`.
///
/// A split module is emitted as a bare `lib.rs` root plus `funcs_{n}.rs` chunks
/// with no build settings of their own, so a consumer who just runs `cargo
/// build --release` would compile at Cargo's defaults and get an inflated
/// binary. This manifest supplies a size-optimized-but-still-fast
/// `[profile.release]`: `opt-level = 3` preserves the flattened-dispatch speed,
/// while thin LTO, a single codegen unit and `strip` shrink the release binary
/// without trading away speed. Panics deliberately keep unwinding (no
/// `panic = "abort"`): Wasm exception handling (including exceptions injected
/// by `resume_throw`) is lowered to `catch_unwind`, and hosts contain traps the
/// same way, which an abort profile would turn into a process abort.
/// The `[lib] path` points at the crate-root `lib.rs` this tool writes
/// (not Cargo's default `src/lib.rs`), and the generated code depends only on
/// `std`, so there is no `[dependencies]` section.
#[must_use]
pub fn cargo_manifest(package_name: &str) -> String {
    format!(
        "[package]\n\
         name = \"{package_name}\"\n\
         version = \"0.1.0\"\n\
         edition = \"2024\"\n\
         \n\
         [lib]\n\
         path = \"lib.rs\"\n\
         \n\
         [profile.release]\n\
         opt-level = 3\n\
         lto = \"thin\"\n\
         codegen-units = 1\n\
         strip = true\n"
    )
}

/// Refuse a WebAssembly component: only core modules are transpiled. Without this
/// the parser would descend into a component's nested modules and emit code for
/// one of them as if it were the whole program.
pub(crate) fn reject_component(encoding: wasmparser::Encoding) -> Result<(), TranspileError> {
    match encoding {
        wasmparser::Encoding::Module => Ok(()),
        wasmparser::Encoding::Component => Err(TranspileError::InvalidInput(
            "WebAssembly components are not supported here; give wasm2rs a core module \
             (for example extract it with `wasm2rs --extract-core`), or transpile the \
             component itself with `wasm2rs --component` (library: `transpile_component`)"
                .into(),
        )),
    }
}

/// Transpile a wasm binary into a single Rust source string.
///
/// Convenience wrapper over [`transpile_split`] with [`SplitOptions::single_file`];
/// use `transpile_split` when a module is large enough that one file is
/// impractical to compile.
pub fn transpile(wasm: &[u8]) -> Result<String, TranspileError> {
    transpile_with_options(wasm, &TranspileOptions::default())
}

/// Transpile a wasm binary, handing each generated [`SourceFile`] to `sink` as
/// soon as it is complete and then dropping it, so the peak memory stays around
/// one file's worth rather than the whole program.
///
/// With [`SplitOptions::single_file`] (or a `funcs_per_file` at least the
/// function count) exactly one `lib.rs` is emitted, identical to [`transpile`].
/// Otherwise the module is split into a `lib.rs` root plus `funcs_{n}.rs` chunk
/// files that together form one Rust crate.
pub fn transpile_split<F>(wasm: &[u8], opts: &SplitOptions, sink: F) -> Result<(), TranspileError>
where
    F: FnMut(SourceFile) -> Result<(), TranspileError>,
{
    transpile_split_impl(wasm, opts, &TranspileOptions::default(), sink)
}

/// Transpile a wasm binary with explicit [`TranspileOptions`] into a single Rust
/// source string — like [`transpile`], but able to opt into codegen options such
/// as [`TranspileOptions::unsafe_memory`].
pub fn transpile_with_options(
    wasm: &[u8],
    opts: &TranspileOptions,
) -> Result<String, TranspileError> {
    let mut code = None;
    transpile_split_impl(wasm, &SplitOptions::single_file(), opts, |file| {
        code = Some(file.code);
        Ok(())
    })?;
    // The single-file path always emits exactly one file.
    code.ok_or_else(|| TranspileError::Internal("no source emitted".into()))
}

/// Transpile a wasm binary into a single Rust source, returned in three
/// consecutive pieces instead of one joined string.
///
/// The concatenation of the pieces is byte-identical to
/// [`transpile_with_options`]; writing them one after another (see
/// [`SourcePieces::write_to`]) avoids copying the bulk of a large output into a
/// second contiguous buffer.
pub fn transpile_pieces_with_options(
    wasm: &[u8],
    opts: &TranspileOptions,
) -> Result<SourcePieces, TranspileError> {
    transpile_impl(wasm, opts, codegen::generate_module_pieces)
}

/// Split-transpile a wasm binary with explicit [`TranspileOptions`], handing each
/// [`SourceFile`] to `sink` as it completes (see [`transpile_split`]). Use this
/// when a large module needs both file splitting and codegen options.
pub fn transpile_split_with_options<F>(
    wasm: &[u8],
    split: &SplitOptions,
    opts: &TranspileOptions,
    sink: F,
) -> Result<(), TranspileError>
where
    F: FnMut(SourceFile) -> Result<(), TranspileError>,
{
    transpile_split_impl(wasm, split, opts, sink)
}

fn transpile_split_impl<F>(
    wasm: &[u8],
    opts: &SplitOptions,
    topts: &TranspileOptions,
    mut sink: F,
) -> Result<(), TranspileError>
where
    F: FnMut(SourceFile) -> Result<(), TranspileError>,
{
    transpile_impl(wasm, topts, |parts| {
        codegen::generate_module_split(
            parts,
            opts.funcs_per_file,
            opts.max_bytes_per_file,
            &mut |name, code| sink(SourceFile { name, code }),
        )
    })
}

/// Reject option combinations the module contradicts, and report whether
/// `reentrant_imports` takes effect.
///
/// The instance is handed to each host call only when there is a host function
/// import to hand it to; the host is lent out of `imports`, so nothing else may
/// read the host while it runs. These checks need the parsed module (an imported
/// memory only exists once the module is read), so they run right after parsing
/// and before any function body is generated.
fn resolve_reentrant_imports(
    topts: &TranspileOptions,
    ir: &parse::ModuleIr<'_>,
) -> Result<bool, TranspileError> {
    let reentrant_imports =
        topts.reentrant_imports && ir.imports.iter().any(|im| im.wasi.is_none());
    if reentrant_imports {
        let host_owned = ir.memories.iter().any(|m| m.imported)
            || ir.table.as_ref().is_some_and(|t| t.imported)
            || !ir.imported_globals.is_empty();
        if host_owned {
            return Err(TranspileError::InvalidOptions(
                "reentrant_imports with an imported memory, table or global".into(),
            ));
        }
        if ir
            .imports
            .iter()
            .any(|im| im.wasi.is_some_and(codegen::WasiFn::spawns_threads))
        {
            return Err(TranspileError::InvalidOptions(
                "reentrant_imports with wasi thread-spawn".into(),
            ));
        }
    }
    Ok(reentrant_imports)
}

/// Parse `wasm`, then hand the resolved module to `generate`.
fn transpile_impl<R>(
    wasm: &[u8],
    topts: &TranspileOptions,
    generate: impl FnOnce(&codegen::ModuleParts<'_>) -> Result<R, TranspileError>,
) -> Result<R, TranspileError> {
    let mut ir = parse::parse_module(wasm, topts)?;
    if topts.named_imports {
        let imports = &mut ir.imports;
        let hosted: Vec<usize> = (0..imports.len())
            .filter(|&i| imports.get(i).is_some_and(|im| im.wasi.is_none()))
            .collect();
        let names = codegen::assign_import_names(hosted.iter().filter_map(|&i| {
            let im = imports.get(i)?;
            Some((
                im.module.as_str(),
                im.field.as_str(),
                u32::try_from(i).ok()?,
            ))
        }));
        for (i, name) in hosted.into_iter().zip(names) {
            if let Some(im) = imports.get_mut(i) {
                im.ident = Some(name);
            }
        }
    }

    let reentrant_imports = resolve_reentrant_imports(topts, &ir)?;

    let funcs = ir.func_inputs()?;
    let live = match &topts.entry_exports {
        Some(entries) => codegen::live_functions(
            &codegen::ReachInput {
                funcs: &funcs,
                imports: &ir.imports,
                exports: &ir.exports,
                elements: &ir.elements,
                global_funcs: &ir.global_funcs,
                start: ir.start,
            },
            entries,
        )?,
        None => vec![true; funcs.len()],
    };

    let parts = codegen::ModuleParts {
        imports: &ir.imports,
        imported_globals: &ir.imported_globals,
        funcs: &funcs,
        live: &live,
        types: &ir.type_sigs,
        type_kinds: ir.type_kinds(),
        supers: ir.supers(),
        type_rel: &ir.type_rel,
        globals: &ir.globals,
        memories: &ir.memories,
        data: &ir.data,
        table: ir.table.as_ref(),
        extra_tables: &ir.extra_tables,
        elements: &ir.elements,
        global_funcs: &ir.global_funcs,
        tags: &ir.tags,
        exports: &ir.exports,
        global_exports: &ir.global_exports,
        start: ir.start,
        opts: topts,
        reentrant_imports,
    };
    generate(&parts)
}
