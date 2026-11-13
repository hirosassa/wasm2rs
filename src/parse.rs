//! Parsing a validated wasm binary into a [`ModuleIr`]: the module-level tables
//! (types, imports, globals, segments, ...) the code generator consumes.
//!
//! Each wasm section is handled by one [`ModuleBuilder`] method; the sections
//! that depend on earlier ones (globals read by segment offsets, table
//! initializers read by globals) rely on the fixed section order of the format.

use crate::{TranspileError, TranspileOptions, codegen, reject_component};
use wasmparser::{
    CompositeInnerType, DataKind, ElementItems, ElementKind, ExternalKind, FuncType, Parser,
    Payload, TableInit, TypeRef, ValType,
};

/// The signature of a wasm function: parameter and result value types.
pub(crate) struct Signature {
    pub(crate) params: Vec<ValType>,
    pub(crate) results: Vec<ValType>,
}

impl Signature {
    /// The placeholder a non-function type holds so the shared type index space
    /// stays aligned for function lookups (never read as a signature).
    fn placeholder() -> Self {
        Self {
            params: Vec::new(),
            results: Vec::new(),
        }
    }
}

/// One entry of the type index space.
pub(crate) struct TypeEntry {
    pub(crate) kind: codegen::CompositeKind,
    pub(crate) sig: Signature,
    /// The declared direct supertype (`None` when the type declares none).
    pub(crate) super_: Option<u32>,
}

/// The type index space. The three views are parallel by construction — entries
/// only enter through [`TypeSpace::push`] — so an index valid in one is valid in
/// all.
#[derive(Default)]
pub(crate) struct TypeSpace {
    pub(crate) signatures: Vec<Signature>,
    /// The kind (function / struct / array / cont) of every type index, so a
    /// concrete reference can resolve its lowering.
    pub(crate) kinds: Vec<codegen::CompositeKind>,
    /// Drives the runtime subtype checks emitted for `ref.test`/`ref.cast`/`br_on_cast`.
    pub(crate) supers: Vec<Option<u32>>,
}

impl TypeSpace {
    pub(crate) fn push(&mut self, entry: TypeEntry) {
        self.signatures.push(entry.sig);
        self.kinds.push(entry.kind);
        self.supers.push(entry.super_);
    }
}

/// A parsed module: everything the code generator needs, resolved and indexed.
pub(crate) struct ModuleIr<'a> {
    pub(crate) types: TypeSpace,
    /// The module's function types, so `call_indirect` can resolve its declared
    /// type index back to a signature.
    pub(crate) type_sigs: Vec<codegen::TypeSig>,
    /// The declared type relation behind funcref `ref.test`/`ref.cast`.
    pub(crate) type_rel: codegen::TypeRel,
    pub(crate) func_type_indices: Vec<u32>,
    pub(crate) bodies: Vec<wasmparser::FunctionBody<'a>>,
    pub(crate) globals: Vec<codegen::GlobalInfo>,
    /// Every linear memory, in index order: imported memories occupy the low
    /// indices (like globals/functions), then defined ones. The multi-memory
    /// proposal allows more than one.
    pub(crate) memories: Vec<codegen::MemInfo>,
    pub(crate) table: Option<codegen::TableInfo>,
    /// Tables 1.. (imported ones first, then defined ones), after table 0.
    pub(crate) extra_tables: Vec<codegen::TableInfo>,
    pub(crate) elements: Vec<codegen::ElemSegment>,
    pub(crate) data: Vec<codegen::DataSegment>,
    pub(crate) imports: Vec<codegen::ImportInfo>,
    /// Host function imports placed after the defined functions in the function index
    /// space (set only by the core linker, which cannot renumber the main module).
    pub(crate) tail_imports: Vec<codegen::ImportInfo>,
    pub(crate) imported_globals: Vec<codegen::ImportedGlobalInfo>,
    /// Exception tags, imported ones first then locally-defined ones, matching
    /// the wasm tag index space.
    pub(crate) tags: Vec<codegen::TagInfo>,
    pub(crate) exports: Vec<codegen::ExportInfo>,
    pub(crate) global_exports: Vec<codegen::GlobalExportInfo>,
    pub(crate) start: Option<u32>,
    /// Functions named by `ref.func` in a global or table initialiser (roots for
    /// `entry_exports`).
    pub(crate) global_funcs: Vec<u32>,
    /// How many of `memories` (0 or 1) are imports, which lead the index space.
    pub(crate) memory_imports: usize,
    /// The initializer expression of each defined global, so the linker can render it
    /// again after renumbering. Empty in a linked module.
    pub(crate) global_inits: Vec<wasmparser::ConstExpr<'a>>,
    /// The integer value of each defined global that is an immutable integer constant
    /// (`None` for any other), so the linker can fold a segment offset that reads one.
    pub(crate) global_ints: Vec<Option<codegen::ConstInt>>,
}

impl<'a> ModuleIr<'a> {
    pub(crate) fn type_kinds(&self) -> &[codegen::CompositeKind] {
        &self.types.kinds
    }

    pub(crate) fn supers(&self) -> &[Option<u32>] {
        &self.types.supers
    }

    /// Resolve each function body against its declared signature.
    pub(crate) fn func_inputs(&self) -> Result<Vec<codegen::FuncInput<'_>>, TranspileError> {
        let mut funcs = Vec::with_capacity(self.bodies.len());
        for (i, body) in self.bodies.iter().enumerate() {
            let type_index = self
                .func_type_indices
                .get(i)
                .copied()
                .ok_or_else(|| TranspileError::Unsupported("missing function type".into()))?;
            let sig = self
                .types
                .signatures
                .get(type_index as usize)
                .ok_or_else(|| {
                    TranspileError::Unsupported("function type index out of range".into())
                })?;
            funcs.push(codegen::FuncInput {
                type_index,
                params: &sig.params,
                results: &sig.results,
                body,
            });
        }
        Ok(funcs)
    }
}

/// Validate and parse `wasm` into a [`ModuleIr`].
pub(crate) fn parse_module<'a>(
    wasm: &'a [u8],
    topts: &TranspileOptions,
) -> Result<ModuleIr<'a>, TranspileError> {
    // Reject ill-typed input up front: the code generator assumes a validated module
    // and would otherwise emit wrong Rust for it (e.g. a mismatched operand stack).
    let types = wasmparser::Validator::new_with_features(wasmparser::WasmFeatures::all())
        .validate_all(wasm)?;
    let mut builder = ModuleBuilder::new(topts);
    for payload in Parser::new(0).parse_all(wasm) {
        builder.payload(payload?)?;
    }
    builder.finish(type_relation(&types)?)
}

/// The accumulators a module is parsed into, plus the state that only matters
/// while parsing.
struct ModuleBuilder<'a, 'o> {
    topts: &'o TranspileOptions,
    types: TypeSpace,
    func_type_indices: Vec<u32>,
    bodies: Vec<wasmparser::FunctionBody<'a>>,
    globals: Vec<codegen::GlobalInfo>,
    memories: Vec<codegen::MemInfo>,
    table: Option<codegen::TableInfo>,
    extra_tables: Vec<codegen::TableInfo>,
    /// Table initializer expressions (`None` = the primary table, `Some(i)` =
    /// `extra_tables[i]`), resolved in `finish` because the table section precedes
    /// the global section.
    pending_table_inits: Vec<(Option<usize>, wasmparser::ConstExpr<'a>)>,
    /// For each defined global, the table-slot handle of its `ref.func`/`ref.null` initializer.
    global_handles: Vec<Option<u32>>,
    /// For each defined global, its integer value if it is an immutable integer constant.
    global_ints: Vec<Option<codegen::ConstInt>>,
    elements: Vec<codegen::ElemSegment>,
    data: Vec<codegen::DataSegment>,
    imports: Vec<codegen::ImportInfo>,
    imported_globals: Vec<codegen::ImportedGlobalInfo>,
    tags: Vec<codegen::TagInfo>,
    exports: Vec<codegen::ExportInfo>,
    global_exports: Vec<codegen::GlobalExportInfo>,
    start: Option<u32>,
    global_funcs: Vec<u32>,
    memory_imports: usize,
    global_inits: Vec<wasmparser::ConstExpr<'a>>,
}

impl<'a, 'o> ModuleBuilder<'a, 'o> {
    fn new(topts: &'o TranspileOptions) -> Self {
        Self {
            topts,
            types: TypeSpace::default(),
            func_type_indices: Vec::new(),
            bodies: Vec::new(),
            globals: Vec::new(),
            memories: Vec::new(),
            table: None,
            extra_tables: Vec::new(),
            pending_table_inits: Vec::new(),
            global_handles: Vec::new(),
            global_ints: Vec::new(),
            elements: Vec::new(),
            data: Vec::new(),
            imports: Vec::new(),
            imported_globals: Vec::new(),
            tags: Vec::new(),
            exports: Vec::new(),
            global_exports: Vec::new(),
            start: None,
            global_funcs: Vec::new(),
            memory_imports: 0,
            global_inits: Vec::new(),
        }
    }

    fn payload(&mut self, payload: Payload<'a>) -> Result<(), TranspileError> {
        match payload {
            Payload::Version { encoding, .. } => reject_component(encoding)?,
            Payload::TypeSection(reader) => self.type_section(reader)?,
            Payload::ImportSection(reader) => self.import_section(reader)?,
            Payload::FunctionSection(reader) => {
                for type_index in reader {
                    self.func_type_indices.push(type_index?);
                }
            }
            Payload::TagSection(reader) => {
                // A tag references a function type whose parameters are its
                // payload (see `tag_info` for the results).
                for tag in reader {
                    self.tags
                        .push(tag_info(tag?.func_type_idx, &self.types.signatures)?);
                }
            }
            Payload::MemorySection(reader) => self.memory_section(reader)?,
            Payload::TableSection(reader) => self.table_section(reader)?,
            Payload::ElementSection(reader) => self.element_section(reader)?,
            Payload::GlobalSection(reader) => self.global_section(reader)?,
            Payload::DataSection(reader) => self.data_section(reader)?,
            Payload::ExportSection(reader) => {
                for export in reader {
                    let export = export?;
                    match export.kind {
                        ExternalKind::Func => self.exports.push(codegen::ExportInfo {
                            name: export.name.to_string(),
                            func_index: export.index,
                        }),
                        ExternalKind::Global => {
                            self.global_exports.push(codegen::GlobalExportInfo {
                                name: export.name.to_string(),
                                global_index: export.index,
                            });
                        }
                        _ => {}
                    }
                }
            }
            Payload::StartSection { func, .. } => self.start = Some(func),
            Payload::CodeSectionEntry(body) => self.bodies.push(body),
            _ => {}
        }
        Ok(())
    }

    fn type_section(
        &mut self,
        reader: wasmparser::TypeSectionReader<'a>,
    ) -> Result<(), TranspileError> {
        for group in reader {
            for sub in group?.types() {
                if sub.supertype_idxs.len() > 1 {
                    return Err(TranspileError::Unsupported(
                        "type with multiple supertypes".into(),
                    ));
                }
                let super_ = sub.supertype_idxs.first().and_then(|p| p.as_module_index());
                // A non-function type occupies a type index too: it records its
                // kind and a placeholder signature.
                let (kind, sig) = match &sub.composite_type.inner {
                    CompositeInnerType::Func(func_ty) => {
                        (codegen::CompositeKind::Func, signature_from(func_ty))
                    }
                    CompositeInnerType::Struct(struct_ty) => {
                        let fields = struct_ty
                            .fields
                            .iter()
                            .map(|f| codegen::FieldInfo {
                                storage: f.element_type,
                            })
                            .collect();
                        (
                            codegen::CompositeKind::Struct(fields),
                            Signature::placeholder(),
                        )
                    }
                    CompositeInnerType::Array(array_ty) => (
                        codegen::CompositeKind::Array(codegen::FieldInfo {
                            storage: array_ty.0.element_type,
                        }),
                        Signature::placeholder(),
                    ),
                    // A continuation type `(cont $ft)` records the underlying
                    // function type index; a `contref` lowers to a `u32` handle
                    // like a funcref.
                    CompositeInnerType::Cont(cont_ty) => {
                        let func_index = cont_ty.0.as_module_index().ok_or_else(|| {
                            TranspileError::Unsupported(
                                "continuation over a non-module type index".into(),
                            )
                        })?;
                        (
                            codegen::CompositeKind::Cont(func_index),
                            Signature::placeholder(),
                        )
                    }
                };
                self.types.push(TypeEntry { kind, sig, super_ });
            }
        }
        Ok(())
    }

    /// Function imports occupy the low end of the function index space
    /// (dispatched through an injected host trait) and imported globals the low
    /// end of the global index space (host getters/setters). Imported memory and
    /// tables are host-owned (lent through the trait); imported tags occupy the
    /// low tag indices. Imports may be grouped in the compact encodings, so each
    /// group is expanded.
    fn import_section(
        &mut self,
        reader: wasmparser::ImportSectionReader<'a>,
    ) -> Result<(), TranspileError> {
        let mut sink = ImportSink {
            imports: &mut self.imports,
            imported_globals: &mut self.imported_globals,
            memories: &mut self.memories,
            table: &mut self.table,
            extra_tables: &mut self.extra_tables,
            tags: &mut self.tags,
        };
        let types = &self.types;
        for group in reader {
            match group? {
                wasmparser::Imports::Single(_, import) => {
                    classify_import((import.module, import.name), import.ty, types, &mut sink)?;
                }
                wasmparser::Imports::Compact1 { module, items } => {
                    for item in items {
                        let item = item?;
                        classify_import((module, item.name), item.ty, types, &mut sink)?;
                    }
                }
                wasmparser::Imports::Compact2 { module, ty, names } => {
                    for name in names {
                        classify_import((module, name?), ty, types, &mut sink)?;
                    }
                }
            }
        }
        // Imports lead the memory index space, so what exists now is all imported.
        self.memory_imports = self.memories.len();
        Ok(())
    }

    fn memory_section(
        &mut self,
        reader: wasmparser::MemorySectionReader<'a>,
    ) -> Result<(), TranspileError> {
        for mem in reader {
            let mem = mem?;
            // `shared` memory (threads proposal) is backed by a thread-shareable
            // `SharedMemory` when it is the module's single defined memory (see
            // codegen atomic ops). A shared memory cannot also be 64-bit: the
            // shared backend is addressed by `u32`.
            if mem.memory64 && mem.shared {
                return Err(TranspileError::Unsupported("shared 64-bit memory".into()));
            }
            self.memories.push(codegen::MemInfo {
                min_pages: mem.initial,
                max_pages: mem.maximum,
                // Only memory 0 can be host-owned, and only when nothing (an
                // imported memory) comes before it.
                imported: self.topts.import_memory && self.memories.is_empty() && !mem.shared,
                shared: mem.shared,
                memory64: mem.memory64,
            });
        }
        Ok(())
    }

    fn table_section(
        &mut self,
        reader: wasmparser::TableSectionReader<'a>,
    ) -> Result<(), TranspileError> {
        for table_entry in reader {
            let table_entry = table_entry?;
            let ty = table_entry.ty;
            if !is_supported_table_element(ty.element_type, &self.types.kinds) {
                return Err(TranspileError::Unsupported(
                    "table of a non-funcref/externref type".into(),
                ));
            }
            if ty.shared {
                return Err(TranspileError::Unsupported("shared table".into()));
            }
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
            let slot = if self.table.is_none() {
                None
            } else {
                Some(self.extra_tables.len())
            };
            // A `ref.null` table starts null (its entries come from active
            // element segments); a `ref.func` one starts filled with that function.
            // An expression is resolved once the globals it may read are known.
            if let TableInit::Expr(expr) = &table_entry.init {
                self.pending_table_inits.push((slot, expr.clone()));
            }
            match slot {
                None => self.table = Some(info),
                Some(_) => self.extra_tables.push(info),
            }
        }
        Ok(())
    }

    fn element_section(
        &mut self,
        reader: wasmparser::ElementSectionReader<'a>,
    ) -> Result<(), TranspileError> {
        let int_globals = codegen::IntGlobals {
            imported: self.imported_globals.len(),
            values: &self.global_ints,
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
                    let n_tables = usize::from(self.table.is_some()) + self.extra_tables.len();
                    if usize::try_from(index).is_ok_and(|i| i >= n_tables) {
                        return Err(TranspileError::Unsupported(
                            "element segment for an unknown table".into(),
                        ));
                    }
                    seg_table = index;
                    let wide = if index == 0 {
                        self.table.as_ref().is_some_and(|t| t.table64)
                    } else {
                        usize::try_from(index - 1)
                            .ok()
                            .and_then(|i| self.extra_tables.get(i))
                            .is_some_and(|t| t.table64)
                    };
                    let constant = if wide {
                        codegen::const_expr_u64_for(offset_expr, "element offset", int_globals)
                    } else {
                        codegen::const_expr_u32(offset_expr, int_globals).map(u64::from)
                    };
                    Some(segment_offset(
                        offset_expr,
                        constant,
                        wide,
                        &self.types.kinds,
                        &self.imported_globals,
                        &self.globals,
                    )?)
                }
                ElementKind::Passive => None,
                // A declared segment has no runtime effect (it only permits
                // `ref.func`), but still occupies an element index, so retain an
                // empty placeholder to keep `table.init`/`elem.drop` indices aligned.
                ElementKind::Declared => {
                    self.elements.push(codegen::ElemSegment {
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
                            &self.imported_globals,
                            &self.global_handles,
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
            self.elements.push(codegen::ElemSegment {
                table: seg_table,
                offset,
                declared: false,
                funcs,
                imported_items,
            });
        }
        Ok(())
    }

    fn global_section(
        &mut self,
        reader: wasmparser::GlobalSectionReader<'a>,
    ) -> Result<(), TranspileError> {
        for global in reader {
            let global = global?;
            for op in global.init_expr.get_operators_reader() {
                if let wasmparser::Operator::RefFunc { function_index } = op? {
                    self.global_funcs.push(function_index);
                }
            }
            let init = codegen::const_expr_to_rust(
                &global.init_expr,
                &self.types.kinds,
                &self.imported_globals,
                &self.globals,
            )?;
            self.global_ints.push(
                codegen::const_int_value(
                    &global.init_expr,
                    codegen::IntGlobals {
                        imported: self.imported_globals.len(),
                        values: &self.global_ints,
                    },
                )
                .filter(|_| !global.ty.mutable),
            );
            // A mutable global is not a constant, so no handle is recorded for it.
            self.global_handles.push(
                eval_ref_const(
                    &global.init_expr,
                    "global",
                    &self.imported_globals,
                    &self.global_handles,
                )
                .ok()
                .and_then(RefConst::handle)
                .filter(|_| !global.ty.mutable),
            );
            self.global_inits.push(global.init_expr.clone());
            self.globals.push(codegen::GlobalInfo {
                ty: global.ty.content_type,
                mutable: global.ty.mutable,
                init,
            });
        }
        Ok(())
    }

    fn data_section(
        &mut self,
        reader: wasmparser::DataSectionReader<'a>,
    ) -> Result<(), TranspileError> {
        let int_globals = codegen::IntGlobals {
            imported: self.imported_globals.len(),
            values: &self.global_ints,
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
                        .and_then(|i| self.memories.get(i))
                        .is_some_and(|m| m.memory64);
                    let constant = codegen::const_expr_u64(offset_expr, int_globals);
                    (
                        Some(segment_offset(
                            offset_expr,
                            constant,
                            wide,
                            &self.types.kinds,
                            &self.imported_globals,
                            &self.globals,
                        )?),
                        *memory_index,
                    )
                }
                DataKind::Passive => (None, 0),
            };
            self.data.push(codegen::DataSegment {
                offset,
                mem_index,
                bytes: segment.data.to_vec(),
            });
        }
        Ok(())
    }

    /// Resolve the table initializers, now that every global they may read is
    /// known. A `ref.func` one is reachable even if the table is never read: it
    /// is seeded at instantiation, like a `ref.func` in a global.
    fn resolve_table_inits(&mut self) -> Result<(), TranspileError> {
        for (slot, expr) in &self.pending_table_inits {
            let init = eval_ref_const(
                expr,
                "table initializer",
                &self.imported_globals,
                &self.global_handles,
            )?;
            let target = match slot {
                None => self.table.as_mut(),
                Some(i) => self.extra_tables.get_mut(*i),
            };
            match init {
                RefConst::Handle(u32::MAX) => {}
                RefConst::Handle(func) => {
                    self.global_funcs.push(func);
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
        Ok(())
    }

    fn finish(mut self, type_rel: codegen::TypeRel) -> Result<ModuleIr<'a>, TranspileError> {
        self.resolve_table_inits()?;
        let type_sigs = self
            .types
            .signatures
            .iter()
            .map(|s| codegen::TypeSig {
                params: s.params.clone(),
                results: s.results.clone(),
            })
            .collect();
        Ok(ModuleIr {
            types: self.types,
            type_sigs,
            type_rel,
            func_type_indices: self.func_type_indices,
            bodies: self.bodies,
            globals: self.globals,
            memories: self.memories,
            table: self.table,
            extra_tables: self.extra_tables,
            elements: self.elements,
            data: self.data,
            imports: self.imports,
            tail_imports: Vec::new(),
            imported_globals: self.imported_globals,
            tags: self.tags,
            exports: self.exports,
            global_exports: self.global_exports,
            start: self.start,
            global_funcs: self.global_funcs,
            memory_imports: self.memory_imports,
            global_inits: self.global_inits,
            global_ints: self.global_ints,
        })
    }
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

/// Whether a table's element type is supported. A table holds `funcref`, `externref`, `exnref`, or a concrete function reference
/// `(ref null $ft)`; the latter is stored exactly like a `funcref` (a `u32`
/// function handle), while an `exnref` (or `noexn`) entry is a managed `GcRef`.
/// An abstract `contref` or bottom type is a `u32` handle too; tables
/// of GC struct/array or concrete continuation references are unsupported.
fn is_supported_table_element(
    element: wasmparser::RefType,
    type_kinds: &[codegen::CompositeKind],
) -> bool {
    if codegen::is_abstract_u32_ref(element) || codegen::is_exn_hierarchy(element.heap_type()) {
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

/// Classify an import by its type: a function import is pushed onto `imports`, a
/// global onto `imported_globals`, an imported memory onto `memories`, an
/// imported table into `table` (or `extra_tables`), and an imported tag onto `tags`.
fn classify_import(
    id: (&str, &str),
    ty: TypeRef,
    types: &TypeSpace,
    sink: &mut ImportSink<'_>,
) -> Result<(), TranspileError> {
    let signatures = &types.signatures;
    let type_kinds = &types.kinds;
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
                alias: None,
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

/// Check a runtime offset's operands: every `global.get` and constant must have the offset's width
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
    let mut reader = expr.get_binary_reader();
    let len = reader.bytes_remaining();
    let bytes = reader.read_bytes(len)?.to_vec();
    Ok(codegen::SegOffset::Runtime {
        rust: if wide {
            format!("({rust}) as u64")
        } else {
            format!("({rust}) as u32 as u64")
        },
        expr: bytes,
        wide,
    })
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

/// The declared type relation of a validated module: each type index's canonical id (a
/// `CoreTypeId`, shared by equivalent types) and each canonical id's declared direct
/// supertype.
fn type_relation(types: &wasmparser::types::Types) -> Result<codegen::TypeRel, TranspileError> {
    let types = types.as_ref();
    let mut ids = std::collections::HashMap::new();
    let mut canon = Vec::new();
    let mut canon_super: Vec<Option<wasmparser::types::CoreTypeId>> = Vec::new();
    for i in 0..types.core_type_count_in_module() {
        let id = types.core_type_at_in_module(i);
        let next = u32::try_from(ids.len())
            .map_err(|_| TranspileError::Unsupported("too many types".into()))?;
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
    Ok(codegen::TypeRel { canon, canon_super })
}
