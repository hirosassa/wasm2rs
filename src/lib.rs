//! wasm2rs: convert a WebAssembly binary into standalone Rust source code.
//!
//! Supported scope: functions whose values are `i32` (params, results, locals),
//! using `i32.const`, `local.get`/`set`/`tee`, `i32` arithmetic/bitwise/compare
//! operators and the structured control-flow instructions `block`, `loop`,
//! `if`/`else`, `br`, `br_if`, `br_table`, `return`, `drop` and `select`.

mod codegen;

use wasmparser::{
    CompositeInnerType, DataKind, ElementItems, ElementKind, FuncType, Parser, Payload, TableInit,
    TypeRef, ValType,
};

/// An error that can occur while transpiling a wasm module.
#[derive(Debug)]
pub enum TranspileError {
    /// The wasm binary could not be parsed.
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

/// The recommended `Cargo.toml` for a split crate, named `package_name`.
///
/// A split module is emitted as a bare `lib.rs` root plus `funcs_{n}.rs` chunks
/// with no build settings of their own, so a consumer who just runs `cargo
/// build --release` would compile at Cargo's defaults and get an inflated
/// binary. This manifest supplies a size-optimized-but-still-fast
/// `[profile.release]`: `opt-level = 3` preserves the flattened-dispatch speed,
/// while thin LTO, a single codegen unit, abort-on-panic (which drops the
/// unwinding tables) and `strip` shrink the release binary without trading away
/// speed. The `[lib] path` points at the crate-root `lib.rs` this tool writes
/// (not Cargo's default `src/lib.rs`), and the generated code depends only on
/// `std`, so there is no `[dependencies]` section.
#[must_use]
pub fn cargo_manifest(package_name: &str) -> String {
    format!(
        "[package]\n\
         name = \"{package_name}\"\n\
         version = \"0.1.0\"\n\
         edition = \"2021\"\n\
         \n\
         [lib]\n\
         path = \"lib.rs\"\n\
         \n\
         [profile.release]\n\
         opt-level = 3\n\
         lto = \"thin\"\n\
         codegen-units = 1\n\
         panic = \"abort\"\n\
         strip = true\n"
    )
}

/// Transpile a wasm binary into a single Rust source string.
///
/// Convenience wrapper over [`transpile_split`] with [`SplitOptions::single_file`];
/// use `transpile_split` when a module is large enough that one file is
/// impractical to compile.
pub fn transpile(wasm: &[u8]) -> Result<String, TranspileError> {
    let mut code = None;
    transpile_split(wasm, &SplitOptions::single_file(), |file| {
        code = Some(file.code);
        Ok(())
    })?;
    // The single-file path always emits exactly one file.
    code.ok_or_else(|| TranspileError::Unsupported("no source emitted".into()))
}

/// Transpile a wasm binary, handing each generated [`SourceFile`] to `sink` as
/// soon as it is complete and then dropping it, so the peak memory stays around
/// one file's worth rather than the whole program.
///
/// With [`SplitOptions::single_file`] (or a `funcs_per_file` at least the
/// function count) exactly one `lib.rs` is emitted, identical to [`transpile`].
/// Otherwise the module is split into a `lib.rs` root plus `funcs_{n}.rs` chunk
/// files that together form one Rust crate.
pub fn transpile_split<F>(
    wasm: &[u8],
    opts: &SplitOptions,
    mut sink: F,
) -> Result<(), TranspileError>
where
    F: FnMut(SourceFile) -> Result<(), TranspileError>,
{
    let mut signatures: Vec<Signature> = Vec::new();
    // The kind (function / struct / array) of every entry in the type index
    // space, parallel to `signatures` so a concrete reference can resolve its
    // lowering. A non-function type still occupies a `signatures` slot (an empty
    // placeholder) to keep the shared index space aligned for func lookups.
    let mut type_kinds: Vec<codegen::CompositeKind> = Vec::new();
    let mut func_type_indices: Vec<u32> = Vec::new();
    let mut bodies: Vec<wasmparser::FunctionBody> = Vec::new();
    let mut globals: Vec<codegen::GlobalInfo> = Vec::new();
    // Every linear memory, in index order: imported memories occupy the low
    // indices (like globals/functions), then defined ones. The multi-memory
    // proposal allows more than one.
    let mut memories: Vec<codegen::MemInfo> = Vec::new();
    let mut table: Option<codegen::TableInfo> = None;
    let mut elements: Vec<codegen::ElemSegment> = Vec::new();
    let mut data: Vec<codegen::DataSegment> = Vec::new();
    let mut imports: Vec<codegen::ImportInfo> = Vec::new();
    let mut imported_globals: Vec<codegen::ImportedGlobalInfo> = Vec::new();
    // Exception tags, imported ones first (classified alongside other imports)
    // then locally-defined ones, matching the wasm tag index space.
    let mut tags: Vec<codegen::TagInfo> = Vec::new();

    for payload in Parser::new(0).parse_all(wasm) {
        match payload? {
            Payload::TypeSection(reader) => {
                for group in reader {
                    for sub in group?.types() {
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
                            _ => {
                                return Err(TranspileError::Unsupported(
                                    "unsupported composite type".into(),
                                ));
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
                    tags: &mut tags,
                };
                for group in reader {
                    match group? {
                        wasmparser::Imports::Single(_, import) => {
                            classify_import(
                                (import.module, import.name),
                                import.ty,
                                &signatures,
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
                                    &mut sink,
                                )?;
                            }
                        }
                        wasmparser::Imports::Compact2 { module, ty, names } => {
                            for name in names {
                                classify_import((module, name?), ty, &signatures, &mut sink)?;
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
                    // single defined memory (see codegen atomic ops). 64-bit
                    // memory is still unsupported.
                    if mem.memory64 {
                        return Err(TranspileError::Unsupported("64-bit memory".into()));
                    }
                    memories.push(codegen::MemInfo {
                        min_pages: mem.initial,
                        imported: false,
                        shared: mem.shared,
                    });
                }
            }
            Payload::TableSection(reader) => {
                for table_entry in reader {
                    let table_entry = table_entry?;
                    let ty = table_entry.ty;
                    if !(ty.element_type.is_func_ref() || ty.element_type.is_extern_ref()) {
                        return Err(TranspileError::Unsupported(
                            "table of a non-funcref/externref type".into(),
                        ));
                    }
                    if ty.table64 || ty.shared {
                        return Err(TranspileError::Unsupported("64-bit or shared table".into()));
                    }
                    if table.is_some() {
                        return Err(TranspileError::Unsupported("multiple tables".into()));
                    }
                    // Only a null-initialised table is supported; its entries
                    // come from active element segments.
                    if !matches!(table_entry.init, TableInit::RefNull) {
                        return Err(TranspileError::Unsupported(
                            "table with an initializer expression".into(),
                        ));
                    }
                    let min = u32::try_from(ty.initial)
                        .map_err(|_| TranspileError::Unsupported("table too large".into()))?;
                    table = Some(codegen::TableInfo {
                        min,
                        imported: false,
                        element: ValType::Ref(ty.element_type),
                    });
                }
            }
            Payload::ElementSection(reader) => {
                for element in reader {
                    let element = element?;
                    let offset = match &element.kind {
                        ElementKind::Active {
                            table_index,
                            offset_expr,
                        } => {
                            if table_index.unwrap_or(0) != 0 {
                                return Err(TranspileError::Unsupported(
                                    "element segment for a non-zero table".into(),
                                ));
                            }
                            Some(codegen::const_expr_u32(offset_expr)?)
                        }
                        ElementKind::Passive => None,
                        // A declared segment has no runtime effect (it only
                        // permits `ref.func`), but still occupies an element
                        // index, so retain an empty placeholder to keep
                        // `table.init`/`elem.drop` indices aligned.
                        ElementKind::Declared => {
                            elements.push(codegen::ElemSegment {
                                offset: None,
                                declared: true,
                                funcs: Vec::new(),
                            });
                            continue;
                        }
                    };
                    let funcs = match element.items {
                        ElementItems::Functions(reader) => {
                            let mut indices = Vec::new();
                            for index in reader {
                                indices.push(index?);
                            }
                            indices
                        }
                        ElementItems::Expressions(..) => {
                            return Err(TranspileError::Unsupported(
                                "element segment with expression items".into(),
                            ));
                        }
                    };
                    elements.push(codegen::ElemSegment {
                        offset,
                        declared: false,
                        funcs,
                    });
                }
            }
            Payload::GlobalSection(reader) => {
                for global in reader {
                    let global = global?;
                    let init = codegen::const_expr_to_rust(&global.init_expr)?;
                    globals.push(codegen::GlobalInfo {
                        ty: global.ty.content_type,
                        mutable: global.ty.mutable,
                        init,
                    });
                }
            }
            Payload::DataSection(reader) => {
                for segment in reader {
                    let segment = segment?;
                    let (offset, mem_index) = match &segment.kind {
                        DataKind::Active {
                            memory_index,
                            offset_expr,
                        } => (Some(codegen::const_expr_u32(offset_expr)?), *memory_index),
                        DataKind::Passive => (None, 0),
                    };
                    data.push(codegen::DataSegment {
                        offset,
                        mem_index,
                        bytes: segment.data.to_vec(),
                    });
                }
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
            params: &sig.params,
            results: &sig.results,
            body,
        });
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

    let parts = codegen::ModuleParts {
        imports: &imports,
        imported_globals: &imported_globals,
        funcs: &funcs,
        types: &types,
        type_kinds: &type_kinds,
        globals: &globals,
        memories: &memories,
        data: &data,
        table: table.as_ref(),
        elements: &elements,
        tags: &tags,
    };
    codegen::generate_module_split(
        &parts,
        opts.funcs_per_file,
        opts.max_bytes_per_file,
        &mut |name, code| sink(SourceFile { name, code }),
    )
}

/// Resolve a tag's referenced function type into its exception payload types.
fn tag_info(
    func_type_idx: u32,
    signatures: &[Signature],
) -> Result<codegen::TagInfo, TranspileError> {
    let sig = signatures
        .get(func_type_idx as usize)
        .ok_or_else(|| TranspileError::Unsupported("tag type index out of range".into()))?;
    Ok(codegen::TagInfo {
        params: sig.params.clone(),
    })
}

/// The module-level accumulators an import is classified into, bundled so the
/// classifier takes one sink rather than many out-parameters.
struct ImportSink<'a> {
    imports: &'a mut Vec<codegen::ImportInfo>,
    imported_globals: &'a mut Vec<codegen::ImportedGlobalInfo>,
    memories: &'a mut Vec<codegen::MemInfo>,
    table: &'a mut Option<codegen::TableInfo>,
    tags: &'a mut Vec<codegen::TagInfo>,
}

/// Classify an import by its type: a function import is pushed onto `imports`, a
/// global onto `imported_globals`, an imported memory recorded in `memory`, an
/// imported table in `table`, and an imported exception tag onto `tags`.
fn classify_import(
    id: (&str, &str),
    ty: TypeRef,
    signatures: &[Signature],
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
                params: sig.params.clone(),
                results: sig.results.clone(),
                wasi,
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
            // See the defined-memory case: 64-bit is not supported. An imported
            // shared memory is out of scope — the thread-shareable backend is
            // only wired up for a single *defined* shared memory.
            if mem_ty.memory64 {
                return Err(TranspileError::Unsupported("64-bit memory".into()));
            }
            if mem_ty.shared {
                return Err(TranspileError::Unsupported("imported shared memory".into()));
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
                imported: true,
                shared: false,
            });
            Ok(())
        }
        TypeRef::Table(table_ty) => {
            if !(table_ty.element_type.is_func_ref() || table_ty.element_type.is_extern_ref()) {
                return Err(TranspileError::Unsupported(
                    "table of a non-funcref/externref type".into(),
                ));
            }
            if table_ty.table64 || table_ty.shared {
                return Err(TranspileError::Unsupported("64-bit or shared table".into()));
            }
            if sink.table.is_some() {
                return Err(TranspileError::Unsupported("multiple tables".into()));
            }
            let min = u32::try_from(table_ty.initial)
                .map_err(|_| TranspileError::Unsupported("table too large".into()))?;
            *sink.table = Some(codegen::TableInfo {
                min,
                imported: true,
                element: ValType::Ref(table_ty.element_type),
            });
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

    /// The lint-suppression attribute prefixed to every generated function.
    const ATTR: &str =
        "#[allow(dead_code, unused_variables, unused_assignments, unused_mut, unused_parens)]\n";

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

#[allow(dead_code, unused_variables, unused_assignments, unused_mut, unused_parens)]
impl Instance {
    pub fn new() -> Self {
        Self {
            g0: 42i32,
        }
    }

    pub fn func0(&mut self) -> i32 {
        self.g0
    }
}";
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
        // The byte literal is wrapped across lines (see `byte_array_literal`),
        // so check the destination slice and the byte sequence separately.
        assert!(rust.contains("m[4..6].copy_from_slice(&["), "{rust}");
        assert!(rust.contains("1u8, 2u8,"), "{rust}");
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
            rust.contains("_ => panic!(\"indirect call type mismatch\")"),
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
