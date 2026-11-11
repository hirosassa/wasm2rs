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

use wasmparser::{
    CompositeInnerType, DataKind, ElementItems, ElementKind, ExternalKind, FuncType, Parser,
    Payload, TableInit, TypeRef, ValType,
};

/// An error that can occur while transpiling a wasm module.
#[derive(Debug)]
pub enum TranspileError {
    /// The wasm binary could not be parsed, or failed validation.
    Parse(wasmparser::BinaryReaderError),
    /// The module used a feature that is not supported yet.
    Unsupported(String),
    /// The operand stack was empty when a value was required, indicating a
    /// malformed module (or a bug in the transpiler).
    StackUnderflow,
}

impl std::fmt::Display for TranspileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse(e) => write!(f, "failed to parse wasm: {e}"),
            Self::Unsupported(what) => write!(f, "unsupported: {what}"),
            Self::StackUnderflow => write!(f, "operand stack underflow"),
        }
    }
}

impl std::error::Error for TranspileError {}

impl From<wasmparser::BinaryReaderError> for TranspileError {
    fn from(e: wasmparser::BinaryReaderError) -> Self {
        Self::Parse(e)
    }
}

impl From<std::fmt::Error> for TranspileError {
    fn from(_: std::fmt::Error) -> Self {
        // Writing into an in-memory `String` is infallible in practice; this
        // conversion exists only to satisfy `write!`'s `Result` contract.
        Self::Unsupported("string formatting failed".into())
    }
}

/// The signature of a wasm function: parameter and result value types.
struct Signature {
    params: Vec<ValType>,
    results: Vec<ValType>,
}

/// One emitted Rust source file of a transpiled module.
///
/// `name` is a file name relative to the output directory (`lib.rs` is the
/// crate root; the chunk files are `funcs_0.rs`, `funcs_1.rs`, ...).
pub struct SourceFile {
    pub name: String,
    pub code: String,
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
#[derive(Debug, Clone, Default)]
pub struct TranspileOptions {
    /// Emit unchecked (`unsafe`) linear-memory access: every load/store and bulk
    /// `memory.*` drops its Rust slice bounds check and reads/writes through
    /// `get_unchecked`, gaining speed. **An out-of-bounds access then becomes
    /// undefined behaviour instead of a wasm trap**, so this is sound only for
    /// modules you trust to stay in bounds. Off by default. Not applied to a
    /// `shared` memory.
    pub unsafe_memory: bool,

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
        wasmparser::Encoding::Component => Err(TranspileError::Unsupported(
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
    code.ok_or_else(|| TranspileError::Unsupported("no source emitted".into()))
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
    // Reject ill-typed input up front: the code generator assumes a validated module
    // and would otherwise emit wrong Rust for it (e.g. a mismatched operand stack).
    wasmparser::Validator::new_with_features(wasmparser::WasmFeatures::all()).validate_all(wasm)?;
    let mut signatures: Vec<Signature> = Vec::new();
    // The kind (function / struct / array) of every entry in the type index
    // space, parallel to `signatures` so a concrete reference can resolve its
    // lowering. A non-function type still occupies a `signatures` slot (an empty
    // placeholder) to keep the shared index space aligned for func lookups.
    let mut type_kinds: Vec<codegen::CompositeKind> = Vec::new();
    // The declared direct supertype of every type index (`None` when a type
    // declares no supertype), parallel to `type_kinds`. Drives the runtime
    // subtype checks emitted for `ref.test`/`ref.cast`/`br_on_cast`.
    let mut supers: Vec<Option<u32>> = Vec::new();
    // The type section run through the validator (types only, never the bodies), for
    // the declared subtype relation behind funcref `ref.test`/`ref.cast`; `None` if
    // it does not validate.
    let mut type_validator = Some(wasmparser::Validator::new_with_features(
        wasmparser::WasmFeatures::all(),
    ));
    let mut func_type_indices: Vec<u32> = Vec::new();
    let mut bodies: Vec<wasmparser::FunctionBody> = Vec::new();
    let mut globals: Vec<codegen::GlobalInfo> = Vec::new();
    // Every linear memory, in index order: imported memories occupy the low
    // indices (like globals/functions), then defined ones. The multi-memory
    // proposal allows more than one.
    let mut memories: Vec<codegen::MemInfo> = Vec::new();
    let mut table: Option<codegen::TableInfo> = None;
    // Tables 1.. (imported ones first, then defined ones), after table 0.
    let mut extra_tables: Vec<codegen::TableInfo> = Vec::new();
    // Table initializer expressions (`None` = the primary table, `Some(i)` = `extra_tables[i]`),
    // resolved after the loop because the table section precedes the global section.
    let mut pending_table_inits: Vec<(Option<usize>, wasmparser::ConstExpr<'_>)> = Vec::new();
    // For each defined global, the table-slot handle of its `ref.func`/`ref.null` initializer.
    let mut global_handles: Vec<Option<u32>> = Vec::new();
    // For each defined global, its integer value if it is an immutable integer constant.
    let mut global_ints: Vec<Option<codegen::ConstInt>> = Vec::new();
    let mut elements: Vec<codegen::ElemSegment> = Vec::new();
    let mut data: Vec<codegen::DataSegment> = Vec::new();
    let mut imports: Vec<codegen::ImportInfo> = Vec::new();
    let mut imported_globals: Vec<codegen::ImportedGlobalInfo> = Vec::new();
    // Exception tags, imported ones first (classified alongside other imports)
    // then locally-defined ones, matching the wasm tag index space.
    let mut tags: Vec<codegen::TagInfo> = Vec::new();
    let mut exports: Vec<codegen::ExportInfo> = Vec::new();
    let mut global_exports: Vec<codegen::GlobalExportInfo> = Vec::new();
    let mut start: Option<u32> = None;
    // Functions named by `ref.func` in a global initialiser (roots for `entry_exports`).
    let mut global_funcs: Vec<u32> = Vec::new();

    for payload in Parser::new(0).parse_all(wasm) {
        match payload? {
            Payload::Version {
                num,
                encoding,
                range,
            } => {
                reject_component(encoding)?;
                feed_validator(&mut type_validator, |v| v.version(num, encoding, &range));
            }
            Payload::TypeSection(reader) => {
                feed_validator(&mut type_validator, |v| v.type_section(&reader));
                for group in reader {
                    for sub in group?.types() {
                        // Record the direct supertype (if any) before matching
                        // the kind, keeping `supers` aligned with the type index
                        // space so a subtype chain can be walked at codegen time.
                        if sub.supertype_idxs.len() > 1 {
                            return Err(TranspileError::Unsupported(
                                "type with multiple supertypes".into(),
                            ));
                        }
                        supers.push(sub.supertype_idxs.first().and_then(|p| p.as_module_index()));
                        match &sub.composite_type.inner {
                            CompositeInnerType::Func(func_ty) => {
                                signatures.push(signature_from(func_ty));
                                type_kinds.push(codegen::CompositeKind::Func);
                            }
                            CompositeInnerType::Struct(struct_ty) => {
                                // A struct/array type occupies a type index too;
                                // record its kind and keep `signatures` aligned
                                // with an empty placeholder (never read as a
                                // function signature).
                                signatures.push(Signature {
                                    params: Vec::new(),
                                    results: Vec::new(),
                                });
                                let fields = struct_ty
                                    .fields
                                    .iter()
                                    .map(|f| codegen::FieldInfo {
                                        storage: f.element_type,
                                    })
                                    .collect();
                                type_kinds.push(codegen::CompositeKind::Struct(fields));
                            }
                            CompositeInnerType::Array(array_ty) => {
                                signatures.push(Signature {
                                    params: Vec::new(),
                                    results: Vec::new(),
                                });
                                type_kinds.push(codegen::CompositeKind::Array(
                                    codegen::FieldInfo {
                                        storage: array_ty.0.element_type,
                                    },
                                ));
                            }
                            CompositeInnerType::Cont(cont_ty) => {
                                // A continuation type `(cont $ft)` occupies a
                                // type index too; record the underlying function
                                // type index and keep `signatures` aligned with
                                // an empty placeholder (never read as a function
                                // signature). A `contref` lowers to a `u32`
                                // handle like a funcref.
                                signatures.push(Signature {
                                    params: Vec::new(),
                                    results: Vec::new(),
                                });
                                let func_index = cont_ty.0.as_module_index().ok_or_else(|| {
                                    TranspileError::Unsupported(
                                        "continuation over a non-module type index".into(),
                                    )
                                })?;
                                type_kinds.push(codegen::CompositeKind::Cont(func_index));
                            }
                        }
                    }
                }
            }
            Payload::ImportSection(reader) => {
                // Function imports occupy the low end of the function index
                // space (dispatched through an injected host trait) and imported
                // globals the low end of the global index space (host getters/
                // setters). Imported memory and tables are host-owned (lent
                // through the trait); imported tags occupy the low tag indices.
                // Imports may be grouped in the compact encodings, so each group
                // is expanded.
                let mut sink = ImportSink {
                    imports: &mut imports,
                    imported_globals: &mut imported_globals,
                    memories: &mut memories,
                    table: &mut table,
                    extra_tables: &mut extra_tables,
                    tags: &mut tags,
                };
                for group in reader {
                    match group? {
                        wasmparser::Imports::Single(_, import) => {
                            classify_import(
                                (import.module, import.name),
                                import.ty,
                                &signatures,
                                &type_kinds,
                                &mut sink,
                            )?;
                        }
                        wasmparser::Imports::Compact1 { module, items } => {
                            for item in items {
                                let item = item?;
                                classify_import(
                                    (module, item.name),
                                    item.ty,
                                    &signatures,
                                    &type_kinds,
                                    &mut sink,
                                )?;
                            }
                        }
                        wasmparser::Imports::Compact2 { module, ty, names } => {
                            for name in names {
                                classify_import(
                                    (module, name?),
                                    ty,
                                    &signatures,
                                    &type_kinds,
                                    &mut sink,
                                )?;
                            }
                        }
                    }
                }
            }
            Payload::FunctionSection(reader) => {
                for type_index in reader {
                    func_type_indices.push(type_index?);
                }
            }
            Payload::TagSection(reader) => {
                // A tag references a function type whose parameters are the
                // exception's payload; exceptions carry no results.
                for tag in reader {
                    tags.push(tag_info(tag?.func_type_idx, &signatures)?);
                }
            }
            Payload::MemorySection(reader) => {
                for mem in reader {
                    let mem = mem?;
                    // `shared` memory (threads proposal) is backed by a
                    // thread-shareable `SharedMemory` when it is the module's
                    // single defined memory (see codegen atomic ops). A shared
                    // memory cannot also be 64-bit: the shared backend is
                    // addressed by `u32`.
                    if mem.memory64 && mem.shared {
                        return Err(TranspileError::Unsupported("shared 64-bit memory".into()));
                    }
                    memories.push(codegen::MemInfo {
                        min_pages: mem.initial,
                        max_pages: mem.maximum,
                        // Only memory 0 can be host-owned, and only when nothing
                        // (an imported memory) comes before it.
                        imported: topts.import_memory && memories.is_empty() && !mem.shared,
                        shared: mem.shared,
                        memory64: mem.memory64,
                    });
                }
            }
            Payload::TableSection(reader) => {
                for table_entry in reader {
                    let table_entry = table_entry?;
                    let ty = table_entry.ty;
                    if !is_supported_table_element(ty.element_type, &type_kinds) {
                        return Err(TranspileError::Unsupported(
                            "table of a non-funcref/externref type".into(),
                        ));
                    }
                    if ty.shared {
                        return Err(TranspileError::Unsupported("shared table".into()));
                    }
                    // A `ref.null` table starts null (its entries come from active
                    // element segments); a `ref.func` one starts filled with that function.
                    // An expression is resolved once the globals it may read are known.
                    let pending = match &table_entry.init {
                        TableInit::RefNull => None,
                        TableInit::Expr(expr) => Some(expr.clone()),
                    };
                    let min = u32::try_from(ty.initial)
                        .map_err(|_| TranspileError::Unsupported("table too large".into()))?;
                    let info = codegen::TableInfo {
                        min,
                        max: ty.maximum.and_then(|m| u32::try_from(m).ok()),
                        imported: false,
                        table64: ty.table64,
                        element: ValType::Ref(ty.element_type),
                        init: None,
                    };
                    let slot = if table.is_none() {
                        None
                    } else {
                        Some(extra_tables.len())
                    };
                    if let Some(expr) = pending {
                        pending_table_inits.push((slot, expr));
                    }
                    if table.is_none() {
                        table = Some(info);
                    } else {
                        extra_tables.push(info);
                    }
                }
            }
            Payload::ElementSection(reader) => {
                let int_globals = codegen::IntGlobals {
                    imported: imported_globals.len(),
                    values: &global_ints,
                };
                for element in reader {
                    let element = element?;
                    let mut seg_table: u32 = 0;
                    let offset = match &element.kind {
                        ElementKind::Active {
                            table_index,
                            offset_expr,
                        } => {
                            let index = table_index.unwrap_or(0);
                            let n_tables = usize::from(table.is_some()) + extra_tables.len();
                            if usize::try_from(index).is_ok_and(|i| i >= n_tables) {
                                return Err(TranspileError::Unsupported(
                                    "element segment for an unknown table".into(),
                                ));
                            }
                            seg_table = index;
                            let is64 = if index == 0 {
                                table.as_ref().is_some_and(|t| t.table64)
                            } else {
                                usize::try_from(index - 1)
                                    .ok()
                                    .and_then(|i| extra_tables.get(i))
                                    .is_some_and(|t| t.table64)
                            };
                            let wide = is64;
                            let constant = if wide {
                                codegen::const_expr_u64_for(
                                    offset_expr,
                                    "element offset",
                                    int_globals,
                                )
                            } else {
                                codegen::const_expr_u32(offset_expr, int_globals).map(u64::from)
                            };
                            Some(segment_offset(
                                offset_expr,
                                constant,
                                wide,
                                &type_kinds,
                                &imported_globals,
                                &globals,
                            )?)
                        }
                        ElementKind::Passive => None,
                        // A declared segment has no runtime effect (it only
                        // permits `ref.func`), but still occupies an element
                        // index, so retain an empty placeholder to keep
                        // `table.init`/`elem.drop` indices aligned.
                        ElementKind::Declared => {
                            elements.push(codegen::ElemSegment {
                                table: 0,
                                offset: None,
                                declared: true,
                                funcs: Vec::new(),
                                imported_items: Vec::new(),
                            });
                            continue;
                        }
                    };
                    let mut imported_items: Vec<(usize, u32)> = Vec::new();
                    let funcs = match element.items {
                        ElementItems::Functions(reader) => {
                            let mut indices = Vec::new();
                            for index in reader {
                                indices.push(index?);
                            }
                            indices
                        }
                        // Expression items (the form a typed-funcref table uses) are
                        // supported when each is a plain `ref.func`, `ref.null` or
                        // `global.get`. An imported global's value is the host's, so
                        // only an active segment (written at instantiation) may read one.
                        ElementItems::Expressions(_, reader) => {
                            let mut indices = Vec::new();
                            for (k, expr) in reader.into_iter().enumerate() {
                                match eval_ref_const(
                                    &expr?,
                                    "element segment item",
                                    &imported_globals,
                                    &global_handles,
                                )? {
                                    RefConst::Handle(h) => indices.push(h),
                                    RefConst::Imported(_) if offset.is_none() => {
                                        return Err(TranspileError::Unsupported(
                                            "element segment item reading an imported global in a passive segment"
                                                .into(),
                                        ));
                                    }
                                    RefConst::Imported(g) => {
                                        imported_items.push((k, g));
                                        indices.push(u32::MAX);
                                    }
                                }
                            }
                            indices
                        }
                    };
                    elements.push(codegen::ElemSegment {
                        table: seg_table,
                        offset,
                        declared: false,
                        funcs,
                        imported_items,
                    });
                }
            }
            Payload::GlobalSection(reader) => {
                for global in reader {
                    let global = global?;
                    for op in global.init_expr.get_operators_reader() {
                        if let wasmparser::Operator::RefFunc { function_index } = op? {
                            global_funcs.push(function_index);
                        }
                    }
                    let init = codegen::const_expr_to_rust(
                        &global.init_expr,
                        &type_kinds,
                        &imported_globals,
                        &globals,
                    )?;
                    global_ints.push(
                        codegen::const_int_value(
                            &global.init_expr,
                            codegen::IntGlobals {
                                imported: imported_globals.len(),
                                values: &global_ints,
                            },
                        )
                        .filter(|_| !global.ty.mutable),
                    );
                    // A mutable global is not a constant, so no handle is recorded for it.
                    global_handles.push(
                        eval_ref_const(
                            &global.init_expr,
                            "global",
                            &imported_globals,
                            &global_handles,
                        )
                        .ok()
                        .and_then(RefConst::handle)
                        .filter(|_| !global.ty.mutable),
                    );
                    globals.push(codegen::GlobalInfo {
                        ty: global.ty.content_type,
                        mutable: global.ty.mutable,
                        init,
                    });
                }
            }
            Payload::DataSection(reader) => {
                let int_globals = codegen::IntGlobals {
                    imported: imported_globals.len(),
                    values: &global_ints,
                };
                for segment in reader {
                    let segment = segment?;
                    let (offset, mem_index) = match &segment.kind {
                        DataKind::Active {
                            memory_index,
                            offset_expr,
                        } => {
                            let wide = usize::try_from(*memory_index)
                                .ok()
                                .and_then(|i| memories.get(i))
                                .is_some_and(|m| m.memory64);
                            let constant = codegen::const_expr_u64(offset_expr, int_globals);
                            (
                                Some(segment_offset(
                                    offset_expr,
                                    constant,
                                    wide,
                                    &type_kinds,
                                    &imported_globals,
                                    &globals,
                                )?),
                                *memory_index,
                            )
                        }
                        DataKind::Passive => (None, 0),
                    };
                    data.push(codegen::DataSegment {
                        offset,
                        mem_index,
                        bytes: segment.data.to_vec(),
                    });
                }
            }
            Payload::ExportSection(reader) => {
                for export in reader {
                    let export = export?;
                    match export.kind {
                        ExternalKind::Func => exports.push(codegen::ExportInfo {
                            name: export.name.to_string(),
                            func_index: export.index,
                        }),
                        ExternalKind::Global => global_exports.push(codegen::GlobalExportInfo {
                            name: export.name.to_string(),
                            global_index: export.index,
                        }),
                        _ => {}
                    }
                }
            }
            Payload::StartSection { func, .. } => {
                start = Some(func);
            }
            Payload::CodeSectionEntry(body) => {
                bodies.push(body);
            }
            _ => {}
        }
    }

    // Resolve each function body against its declared signature.
    let mut funcs: Vec<codegen::FuncInput> = Vec::with_capacity(bodies.len());
    for (i, body) in bodies.iter().enumerate() {
        let type_index = func_type_indices
            .get(i)
            .copied()
            .ok_or_else(|| TranspileError::Unsupported("missing function type".into()))?;
        let sig = signatures.get(type_index as usize).ok_or_else(|| {
            TranspileError::Unsupported("function type index out of range".into())
        })?;
        funcs.push(codegen::FuncInput {
            type_index,
            params: &sig.params,
            results: &sig.results,
            body,
        });
    }

    // Table initializers, now that every global they may read is known. A
    // `ref.func` one is reachable even if the table is never read: it is seeded at
    // instantiation, like a `ref.func` in a global.
    for (slot, expr) in &pending_table_inits {
        let init = eval_ref_const(
            expr,
            "table initializer",
            &imported_globals,
            &global_handles,
        )?;
        let target = match slot {
            None => table.as_mut(),
            Some(i) => extra_tables.get_mut(*i),
        };
        match init {
            RefConst::Handle(u32::MAX) => {}
            RefConst::Handle(func) => {
                global_funcs.push(func);
                if let Some(target) = target {
                    target.init = Some(codegen::TableFill::Func(func));
                }
            }
            RefConst::Imported(global) => {
                if let Some(target) = target {
                    target.init = Some(codegen::TableFill::ImportedGlobal(global));
                }
            }
        }
    }

    // The module's function types, so `call_indirect` can resolve its declared
    // type index back to a signature.
    let types: Vec<codegen::TypeSig> = signatures
        .iter()
        .map(|s| codegen::TypeSig {
            params: s.params.clone(),
            results: s.results.clone(),
        })
        .collect();

    if topts.named_imports {
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

    let live = match &topts.entry_exports {
        Some(entries) => codegen::live_functions(
            &codegen::ReachInput {
                funcs: &funcs,
                imports: &imports,
                exports: &exports,
                elements: &elements,
                global_funcs: &global_funcs,
                start,
            },
            entries,
        )?,
        None => vec![true; funcs.len()],
    };

    // The instance is handed to each host call only when there is a host function
    // import to hand it to; the host is lent out of `imports`, so nothing else may
    // read the host while it runs.
    let reentrant_imports = topts.reentrant_imports && imports.iter().any(|im| im.wasi.is_none());
    if reentrant_imports {
        let host_owned = memories.iter().any(|m| m.imported)
            || table.as_ref().is_some_and(|t| t.imported)
            || !imported_globals.is_empty();
        if host_owned {
            return Err(TranspileError::Unsupported(
                "reentrant_imports with an imported memory, table or global".into(),
            ));
        }
        if imports
            .iter()
            .any(|im| im.wasi.is_some_and(codegen::WasiFn::spawns_threads))
        {
            return Err(TranspileError::Unsupported(
                "reentrant_imports with wasi thread-spawn".into(),
            ));
        }
    }
    let type_rel = type_validator.as_ref().and_then(type_relation);
    let parts = codegen::ModuleParts {
        imports: &imports,
        imported_globals: &imported_globals,
        funcs: &funcs,
        live: &live,
        types: &types,
        type_kinds: &type_kinds,
        supers: &supers,
        type_rel: type_rel.as_ref(),
        globals: &globals,
        memories: &memories,
        data: &data,
        table: table.as_ref(),
        extra_tables: &extra_tables,
        elements: &elements,
        global_funcs: &global_funcs,
        tags: &tags,
        exports: &exports,
        global_exports: &global_exports,
        start,
        unsafe_memory: topts.unsafe_memory,
        reentrant_imports,
        split_dispatch: topts.split_dispatch,
        jobs: topts.jobs,
        named_exports: topts.named_exports,
    };
    codegen::generate_module_split(
        &parts,
        opts.funcs_per_file,
        opts.max_bytes_per_file,
        &mut |name, code| sink(SourceFile { name, code }),
    )
}

/// Resolve a tag's referenced function type into its parameter and result
/// types (results are empty for an exception tag, non-empty for a
/// stack-switching control tag).
fn tag_info(
    func_type_idx: u32,
    signatures: &[Signature],
) -> Result<codegen::TagInfo, TranspileError> {
    let sig = signatures
        .get(func_type_idx as usize)
        .ok_or_else(|| TranspileError::Unsupported("tag type index out of range".into()))?;
    Ok(codegen::TagInfo {
        params: sig.params.clone(),
        results: sig.results.clone(),
    })
}

/// The module-level accumulators an import is classified into, bundled so the
/// classifier takes one sink rather than many out-parameters.
struct ImportSink<'a> {
    imports: &'a mut Vec<codegen::ImportInfo>,
    imported_globals: &'a mut Vec<codegen::ImportedGlobalInfo>,
    memories: &'a mut Vec<codegen::MemInfo>,
    table: &'a mut Option<codegen::TableInfo>,
    extra_tables: &'a mut Vec<codegen::TableInfo>,
    tags: &'a mut Vec<codegen::TagInfo>,
}

/// Classify an import by its type: a function import is pushed onto `imports`, a
/// global onto `imported_globals`, an imported memory recorded in `memory`, an
/// imported table in `table`, and an imported exception tag onto `tags`.
/// A table holds `funcref`, `externref`, `exnref`, or a concrete function reference
/// `(ref null $ft)`; the latter is stored exactly like a `funcref` (a `u32`
/// function handle). An abstract `contref` or bottom type is a `u32` handle too; tables
/// of GC struct/array or concrete continuation references are unsupported.
fn is_supported_table_element(
    element: wasmparser::RefType,
    type_kinds: &[codegen::CompositeKind],
) -> bool {
    if codegen::is_abstract_u32_ref(element) {
        return true;
    }
    let wasmparser::HeapType::Concrete(idx) = element.heap_type() else {
        return false;
    };
    matches!(
        idx.as_module_index()
            .and_then(|i| type_kinds.get(i as usize)),
        Some(codegen::CompositeKind::Func)
    )
}

fn classify_import(
    id: (&str, &str),
    ty: TypeRef,
    signatures: &[Signature],
    type_kinds: &[codegen::CompositeKind],
    sink: &mut ImportSink<'_>,
) -> Result<(), TranspileError> {
    match ty {
        TypeRef::Func(type_index) | TypeRef::FuncExact(type_index) => {
            let sig = signatures.get(type_index as usize).ok_or_else(|| {
                TranspileError::Unsupported("import type index out of range".into())
            })?;
            // A recognised WASI function is generated natively; any other import
            // is dispatched through the injected host trait.
            let (module, name) = id;
            let wasi = codegen::WasiFn::recognise(module, name, &sig.params, &sig.results);
            sink.imports.push(codegen::ImportInfo {
                type_index,
                params: sig.params.clone(),
                results: sig.results.clone(),
                wasi,
                module: module.to_string(),
                field: name.to_string(),
                ident: None,
            });
            Ok(())
        }
        TypeRef::Global(global_ty) => {
            sink.imported_globals.push(codegen::ImportedGlobalInfo {
                ty: global_ty.content_type,
                mutable: global_ty.mutable,
            });
            Ok(())
        }
        TypeRef::Memory(mem_ty) => {
            // An imported shared memory is a host-created `SharedMemory` handed
            // to `Instance::new`; like a defined one it is addressed by `u32`.
            if mem_ty.shared && mem_ty.memory64 {
                return Err(TranspileError::Unsupported("shared 64-bit memory".into()));
            }
            // A host-lent buffer is only wired up for memory 0 (`Imports::memory`);
            // an imported memory at a higher index (rare, out of scope) is
            // rejected. Imports precede defined memories, so this is the first
            // memory when the list is still empty.
            if !sink.memories.is_empty() {
                return Err(TranspileError::Unsupported(
                    "imported non-zero memory".into(),
                ));
            }
            sink.memories.push(codegen::MemInfo {
                min_pages: mem_ty.initial,
                max_pages: mem_ty.maximum,
                imported: true,
                shared: mem_ty.shared,
                memory64: mem_ty.memory64,
            });
            Ok(())
        }
        TypeRef::Table(table_ty) => {
            if !is_supported_table_element(table_ty.element_type, type_kinds) {
                return Err(TranspileError::Unsupported(
                    "table of a non-funcref/externref type".into(),
                ));
            }
            if table_ty.shared {
                return Err(TranspileError::Unsupported("imported shared table".into()));
            }
            let min = u32::try_from(table_ty.initial)
                .map_err(|_| TranspileError::Unsupported("table too large".into()))?;
            let info = codegen::TableInfo {
                min,
                max: table_ty.maximum.and_then(|m| u32::try_from(m).ok()),
                imported: true,
                table64: table_ty.table64,
                element: ValType::Ref(table_ty.element_type),
                init: None,
            };
            // The first table (imported or not) is table 0; imports come first in
            // the index space, so any further imported table is `extra_tables[i - 1]`.
            if sink.table.is_none() {
                *sink.table = Some(info);
            } else {
                sink.extra_tables.push(info);
            }
            Ok(())
        }
        TypeRef::Tag(tag_ty) => {
            sink.tags.push(tag_info(tag_ty.func_type_idx, signatures)?);
            Ok(())
        }
    }
}

fn signature_from(func_ty: &FuncType) -> Signature {
    Signature {
        params: func_ty.params().to_vec(),
        results: func_ty.results().to_vec(),
    }
}

/// Whether a constant expression reads an imported global (its value is the host's).
fn reads_imported_global(
    expr: &wasmparser::ConstExpr<'_>,
    imported: usize,
) -> Result<bool, TranspileError> {
    for op in expr.get_operators_reader() {
        if let wasmparser::Operator::GlobalGet { global_index } = op?
            && usize::try_from(global_index).is_ok_and(|i| i < imported)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The transpiler does not run the validator, so a runtime offset's operands are
/// checked here: every `global.get` and constant must have the offset's width
/// (`i64` when `wide`, else `i32`), as the constant path insists.
fn check_offset_width(
    expr: &wasmparser::ConstExpr<'_>,
    wide: bool,
    imported_globals: &[codegen::ImportedGlobalInfo],
    globals: &[codegen::GlobalInfo],
) -> Result<(), TranspileError> {
    use wasmparser::Operator::{
        GlobalGet, I32Add, I32Const, I32Mul, I32Sub, I64Add, I64Const, I64Mul, I64Sub,
    };
    let width = if wide { ValType::I64 } else { ValType::I32 };
    let mismatch = || {
        TranspileError::Unsupported(format!(
            "segment offset: operand type mismatch (expected {width})"
        ))
    };
    for op in expr.get_operators_reader() {
        match op? {
            GlobalGet { global_index } => {
                let index = usize::try_from(global_index).ok();
                let ty = index.and_then(|i| match i.checked_sub(imported_globals.len()) {
                    Some(defined) => globals.get(defined).map(|g| g.ty),
                    None => imported_globals.get(i).map(|g| g.ty),
                });
                if ty != Some(width) {
                    return Err(mismatch());
                }
            }
            I32Const { .. } | I32Add | I32Sub | I32Mul if wide => return Err(mismatch()),
            I64Const { .. } | I64Add | I64Sub | I64Mul if !wide => return Err(mismatch()),
            _ => {}
        }
    }
    Ok(())
}

/// An active segment's offset. `constant` is its value evaluated now; when that
/// failed because the expression reads an imported global (only the host knows it),
/// the offset becomes a Rust expression evaluated when the instance is built.
/// `wide` says whether the offset is an `i64` (a 64-bit memory or table).
fn segment_offset(
    expr: &wasmparser::ConstExpr<'_>,
    constant: Result<u64, TranspileError>,
    wide: bool,
    type_kinds: &[codegen::CompositeKind],
    imported_globals: &[codegen::ImportedGlobalInfo],
    globals: &[codegen::GlobalInfo],
) -> Result<codegen::SegOffset, TranspileError> {
    let err = match constant {
        Ok(value) => return Ok(codegen::SegOffset::Const(value)),
        Err(err) => err,
    };
    if !reads_imported_global(expr, imported_globals.len())? {
        return Err(err);
    }
    check_offset_width(expr, wide, imported_globals, globals)?;
    let rust = codegen::const_expr_to_rust(expr, type_kinds, imported_globals, globals)?;
    Ok(codegen::SegOffset::Runtime(if wide {
        format!("({rust}) as u64")
    } else {
        format!("({rust}) as u32 as u64")
    }))
}

/// What a constant reference expression denotes: a function index (`u32::MAX`
/// for null) known at transpile time, or an immutable imported global whose value
/// only the host knows.
enum RefConst {
    Handle(u32),
    Imported(u32),
}

impl RefConst {
    fn handle(self) -> Option<u32> {
        match self {
            RefConst::Handle(h) => Some(h),
            RefConst::Imported(_) => None,
        }
    }
}

/// Evaluate a constant reference expression: the function index for `ref.func`,
/// `u32::MAX` (null) for `ref.null`, or, for `global.get`, the handle of an
/// earlier defined global (`global_handles`, indexed after the imported ones) or
/// an immutable imported global. Anything else is unsupported.
fn eval_ref_const(
    expr: &wasmparser::ConstExpr<'_>,
    what: &str,
    imported_globals: &[codegen::ImportedGlobalInfo],
    global_handles: &[Option<u32>],
) -> Result<RefConst, TranspileError> {
    let mut ops = expr.get_operators_reader();
    match (ops.read()?, ops.read()?) {
        (wasmparser::Operator::RefFunc { function_index }, wasmparser::Operator::End) => {
            Ok(RefConst::Handle(function_index))
        }
        (wasmparser::Operator::RefNull { .. }, wasmparser::Operator::End) => {
            Ok(RefConst::Handle(u32::MAX))
        }
        (wasmparser::Operator::GlobalGet { global_index }, wasmparser::Operator::End) => {
            let constant = usize::try_from(global_index).ok().and_then(|i| {
                match i.checked_sub(imported_globals.len()) {
                    Some(defined) => global_handles
                        .get(defined)
                        .copied()
                        .flatten()
                        .map(RefConst::Handle),
                    // Only an immutable reference global is a constant.
                    None => imported_globals
                        .get(i)
                        .filter(|g| !g.mutable && matches!(g.ty, ValType::Ref(_)))
                        .map(|_| RefConst::Imported(global_index)),
                }
            });
            constant.ok_or_else(|| {
                TranspileError::Unsupported(format!(
                    "{what} reading a mutable, non-reference or non-constant global"
                ))
            })
        }
        _ => Err(TranspileError::Unsupported(format!(
            "{what} that is not ref.func, ref.null or global.get"
        ))),
    }
}

/// Run one step on the type validator, dropping it (and with it the type relation) if
/// the step fails.
fn feed_validator(
    validator: &mut Option<wasmparser::Validator>,
    step: impl FnOnce(&mut wasmparser::Validator) -> wasmparser::Result<()>,
) {
    if validator.as_mut().is_some_and(|v| step(v).is_err()) {
        *validator = None;
    }
}

/// The declared type relation of a validator that has read the type section: each type
/// index's canonical id (a `CoreTypeId`, shared by equivalent types) and each canonical
/// id's declared direct supertype.
fn type_relation(validator: &wasmparser::Validator) -> Option<codegen::TypeRel> {
    let types = validator.types(0)?;
    let mut ids = std::collections::HashMap::new();
    let mut canon = Vec::new();
    let mut canon_super: Vec<Option<wasmparser::types::CoreTypeId>> = Vec::new();
    for i in 0..types.core_type_count_in_module() {
        let id = types.core_type_at_in_module(i);
        let next = u32::try_from(ids.len()).ok()?;
        let dense = *ids.entry(id).or_insert_with(|| {
            canon_super.push(
                types[id]
                    .supertype_idxs
                    .first()
                    .and_then(|p| p.as_core_type_id()),
            );
            next
        });
        canon.push(dense);
    }
    let canon_super = canon_super
        .into_iter()
        .map(|sup| sup.and_then(|id| ids.get(&id).copied()))
        .collect();
    Some(codegen::TypeRel { canon, canon_super })
}

#[cfg(test)]
mod tests {
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
    use super::*;

    fn wat_to_wasm(wat: &str) -> Vec<u8> {
        wat::parse_str(wat).expect("valid wat")
    }

    /// The lint-suppression attribute prefixed to every generated function. It is a
    /// literal because the tests below splice it into `format!` strings by name;
    /// `attr_matches_the_generators_lint_list` keeps it in sync with the generator.
    const ATTR: &str = "#[allow(dead_code, unused_variables, unused_assignments, unused_mut, unused_parens, unconditional_recursion, unconditional_panic)]\n";

    #[test]
    fn attr_matches_the_generators_lint_list() {
        assert_eq!(ATTR, format!("{}\n", crate::codegen::ALLOW));
    }

    #[test]
    fn i32_add_becomes_wrapping_add() {
        let wasm = wat_to_wasm(
            r#"
            (module
              (func (param i32 i32) (result i32)
                local.get 0
                local.get 1
                i32.add))
            "#,
        );

        let rust = transpile(&wasm).expect("transpile ok");

        assert_eq!(
            rust.trim(),
            format!(
                "{ATTR}pub fn func0(l0: i32, l1: i32) -> i32 {{\n    (l0).wrapping_add(l1)\n}}"
            ),
        );
    }

    #[test]
    fn i32_const_and_mul() {
        let wasm = wat_to_wasm(
            r#"
            (module
              (func (result i32)
                i32.const 2
                i32.const 3
                i32.mul))
            "#,
        );

        let rust = transpile(&wasm).expect("transpile ok");

        assert_eq!(
            rust.trim(),
            format!("{ATTR}pub fn func0() -> i32 {{\n    (2i32).wrapping_mul(3i32)\n}}"),
        );
    }

    #[test]
    fn bitwise_and_is_parenthesized_infix() {
        let wasm = wat_to_wasm(
            r#"
            (module
              (func (param i32 i32) (result i32)
                local.get 0
                local.get 1
                i32.and))
            "#,
        );

        let rust = transpile(&wasm).expect("transpile ok");

        assert_eq!(
            rust.trim(),
            format!("{ATTR}pub fn func0(l0: i32, l1: i32) -> i32 {{\n    (l0 & l1)\n}}"),
        );
    }

    #[test]
    fn multiple_functions_get_sequential_names() {
        let wasm = wat_to_wasm(
            r#"
            (module
              (func (param i32) (result i32) local.get 0)
              (func (param i32 i32) (result i32)
                local.get 0
                local.get 1
                i32.sub))
            "#,
        );

        let rust = transpile(&wasm).expect("transpile ok");

        assert_eq!(
            rust.trim(),
            format!(
                "{ATTR}pub fn func0(l0: i32) -> i32 {{\n    l0\n}}\n\n\
                 {ATTR}pub fn func1(l0: i32, l1: i32) -> i32 {{\n    (l0).wrapping_sub(l1)\n}}"
            ),
        );
    }

    #[test]
    fn i32_min_const_uses_valid_literal() {
        // `-2147483648i32` is a range error in Rust (parsed as unary minus on
        // an out-of-range literal), so `i32::MIN` must be emitted specially.
        let wasm = wat_to_wasm(
            r#"
            (module
              (func (result i32)
                i32.const -2147483648))
            "#,
        );

        let rust = transpile(&wasm).expect("transpile ok");

        assert_eq!(
            rust.trim(),
            format!("{ATTR}pub fn func0() -> i32 {{\n    i32::MIN\n}}"),
        );
    }

    #[test]
    fn imported_function_generates_trait_and_generic_instance() {
        let wasm = wat_to_wasm(
            r#"
            (module
              (import "env" "ext" (func (param i32) (result i32)))
              (func (param i32) (result i32) (call 0 (local.get 0))))
            "#,
        );

        let rust = transpile(&wasm).expect("transpile ok");

        // The import becomes a trait method and `Instance` is generic over the
        // host; the defined function is index 1 and dispatches into the host.
        assert!(
            rust.contains("pub trait Imports {")
                && rust.contains("fn import0(&mut self, a0: i32) -> i32;"),
            "{rust}"
        );
        assert!(rust.contains("pub struct Instance<H: Imports> {"), "{rust}");
        assert!(rust.contains("imports: H,"), "{rust}");
        assert!(
            rust.contains("pub fn func1(&mut self, l0: i32) -> i32")
                && rust.contains("self.imports.import0(l0)"),
            "{rust}"
        );
    }

    #[test]
    fn imported_memory_is_supported() {
        // Imported memory is host-owned; the module accesses it through the
        // injected `Imports` trait (see tests/imported_memory.rs for behaviour).
        let wasm = wat_to_wasm(
            r#"
            (module
              (import "env" "mem" (memory 1))
              (func (param i32) (result i32) (i32.load8_u (local.get 0))))
            "#,
        );

        let rust = transpile(&wasm).expect("imported memory should transpile");
        assert!(
            rust.contains("fn memory(&self) -> &[u8];") && rust.contains("self.imports.memory()"),
            "{rust}"
        );
    }

    #[test]
    fn imported_table_lends_storage_through_the_host() {
        // An imported table is host-owned: the trait declares `table`/`table_mut`
        // accessors and the instance routes through them (no `table` field).
        let wasm = wat_to_wasm(
            r#"
            (module
              (import "env" "t" (table 1 funcref))
              (func (param i32) (result i32) local.get 0))
            "#,
        );

        let rust = transpile(&wasm).expect("imported table should transpile");
        assert!(
            rust.contains("fn table(&self) -> &[u32];") && rust.contains("self.imports.table()"),
            "{rust}"
        );
    }

    #[test]
    fn result_declared_but_empty_stack_is_rejected() {
        // A function that declares a result but falls off its end with nothing
        // on the stack must be an error, rather than silently emitting an
        // empty-bodied `-> i32` function that does not compile.
        let wasm = wat_to_wasm(
            r#"
            (module
              (func (result i32) (nop)))
            "#,
        );

        assert!(transpile(&wasm).is_err());
    }

    #[test]
    fn unreachable_body_transpiles_to_a_trap() {
        // `unreachable` terminates the function, so a declared result needs no
        // trailing value; the body is a panic (a wasm trap).
        let wasm = wat_to_wasm(
            r#"
            (module
              (func (result i32) (unreachable)))
            "#,
        );

        let rust = transpile(&wasm).expect("transpile ok");
        assert!(rust.contains("panic!(\"unreachable\")"), "{rust}");
    }

    #[test]
    fn i32_eq_uses_i32_from() {
        let wasm = wat_to_wasm(
            r#"
            (module
              (func (param i32 i32) (result i32)
                (i32.eq (local.get 0) (local.get 1))))
            "#,
        );

        let rust = transpile(&wasm).expect("transpile ok");

        assert_eq!(
            rust.trim(),
            format!("{ATTR}pub fn func0(l0: i32, l1: i32) -> i32 {{\n    i32::from(l0 == l1)\n}}"),
        );
    }

    #[test]
    fn i32_lt_u_casts_operands_to_u32() {
        let wasm = wat_to_wasm(
            r#"
            (module
              (func (param i32 i32) (result i32)
                (i32.lt_u (local.get 0) (local.get 1))))
            "#,
        );

        let rust = transpile(&wasm).expect("transpile ok");

        assert_eq!(
            rust.trim(),
            format!(
                "{ATTR}pub fn func0(l0: i32, l1: i32) -> i32 {{\n    i32::from((l0 as u32) < (l1 as u32))\n}}"
            ),
        );
    }

    #[test]
    fn declared_local_is_initialized_and_assigned() {
        let wasm = wat_to_wasm(
            r#"
            (module
              (func (param i32) (result i32) (local i32)
                (local.set 1 (local.get 0))
                (local.get 1)))
            "#,
        );

        let rust = transpile(&wasm).expect("transpile ok");

        assert_eq!(
            rust.trim(),
            format!(
                "{ATTR}pub fn func0(l0: i32) -> i32 {{\n    let mut l1: i32 = 0;\n    l1 = l0;\n    l1\n}}"
            ),
        );
    }

    #[test]
    fn module_with_global_emits_instance_struct() {
        let wasm = wat_to_wasm(
            r#"
            (module
              (global i32 (i32.const 42))
              (func (result i32) (global.get 0)))
            "#,
        );

        let rust = transpile(&wasm).expect("transpile ok");

        let expected = "\
#[allow(dead_code)]
pub struct Instance {
    g0: i32,
}

{ALLOW}
impl Instance {
    pub fn new() -> Self {
        Self {
            g0: 42i32,
        }
    }

    pub fn func0(&mut self) -> i32 {
        self.g0
    }
}"
        .replace("{ALLOW}", crate::codegen::ALLOW);
        assert_eq!(rust.trim(), expected);
    }

    #[test]
    fn direct_call_materialises_result_into_temp() {
        let wasm = wat_to_wasm(
            r#"
            (module
              (func (param i32 i32) (result i32) (i32.add (local.get 0) (local.get 1)))
              (func (param i32) (result i32) (call 0 (local.get 0) (i32.const 10))))
            "#,
        );

        let rust = transpile(&wasm).expect("transpile ok");

        assert_eq!(
            rust.trim(),
            format!(
                "{ATTR}pub fn func0(l0: i32, l1: i32) -> i32 {{\n    (l0).wrapping_add(l1)\n}}\n\n\
                 {ATTR}pub fn func1(l0: i32) -> i32 {{\n    \
                 let v0: i32 = func0(l0, 10i32);\n    v0\n}}"
            ),
        );
    }

    #[test]
    fn active_data_segment_copies_bytes_in_new() {
        let wasm = wat_to_wasm(
            r#"
            (module
              (memory 1)
              (data (i32.const 4) "\01\02")
              (func (param i32) (result i32) (i32.load8_u (local.get 0))))
            "#,
        );

        let rust = transpile(&wasm).expect("transpile ok");

        // `new()` zeroes the memory then copies the segment bytes into place.
        assert!(
            rust.contains("let mut m: Vec<u8> = vec![0u8; 65536];"),
            "{rust}"
        );
        // The bytes live in a module-scope `static` (a single string literal),
        // and `new()` copies from it.
        assert!(
            rust.contains("static DATA0: [u8; 2] = *b\"\\x01\\x02\";"),
            "{rust}"
        );
        assert!(rust.contains("m[4..6].copy_from_slice(&DATA0);"), "{rust}");
    }

    #[test]
    fn active_data_source_stays_compact() {
        let segment = "\\ff".repeat(4096);
        let wasm = wat_to_wasm(&format!(
            r#"(module (memory 1) (data (i32.const 0) "{segment}"))"#
        ));

        let rust = transpile(&wasm).expect("transpile ok");

        // One escaped byte costs 4 characters, so the segment's static stays
        // under 5 per byte (measured on its own line, not the whole module).
        let start = rust.find("static DATA0").expect("DATA0 static");
        let len = rust[start..].find("\";").expect("end of DATA0 static");
        assert!(len < 4096 * 5, "{len} bytes for the static");
    }

    #[test]
    fn call_indirect_dispatches_through_table_match() {
        let wasm = wat_to_wasm(
            r#"
            (module
              (type $unary (func (param i32) (result i32)))
              (table 1 funcref)
              (elem (i32.const 0) $id)
              (func $id (param i32) (result i32) (local.get 0))
              (func $go (param i32 i32) (result i32)
                (call_indirect (type $unary) (local.get 1) (local.get 0))))
            "#,
        );

        let rust = transpile(&wasm).expect("transpile ok");

        // The table is a `Vec<u32>` of function indices seeded from the element
        // segment; dispatch reads the entry through the `table()` accessor into a
        // local, then a `match` traps on a null/wrong entry.
        assert!(rust.contains("table: Vec<u32>,"), "{rust}");
        assert!(rust.contains("t[0] = 0u32;"), "{rust}");
        assert!(
            rust.contains("= self.table()[") && rust.contains("0u32 => self.func0("),
            "{rust}"
        );
        assert!(
            rust.contains("_ => trap_indirect_type_mismatch(),")
                && rust.contains("fn trap_indirect_type_mismatch() -> ! {"),
            "{rust}"
        );
    }

    #[test]
    fn function_without_result_has_no_return_type() {
        let wasm = wat_to_wasm(
            r#"
            (module
              (func (param i32)))
            "#,
        );

        let rust = transpile(&wasm).expect("transpile ok");

        assert_eq!(rust.trim(), format!("{ATTR}pub fn func0(l0: i32) {{\n}}"));
    }
}
