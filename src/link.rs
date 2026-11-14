//! Linking several core modules into one [`ModuleIr`] (see
//! `docs/core-module-linking-design.md`, stage L2).
//!
//! The first module is the *main* one and keeps every index space unchanged, so its bodies
//! (megabytes for a real program) are never rewritten. The others are appended: their
//! types, functions, globals, tables, memories and segments get an offset, and their bodies
//! are copied with only the index immediates re-encoded. An import of the main module that
//! another module answers keeps its slot as an *alias* ([`codegen::ImportInfo::alias`]), so
//! the main module's function indices stay valid while a call becomes a direct one.
//!
//! This is a purely core-level step: the caller says, for each import, what provides it
//! ([`Source`]); nothing here knows about components.

use std::collections::HashMap;

use wasm_encoder::Encode;
use wasm_encoder::reencode::{self, Reencode};
use wasmparser::{BinaryReader, ConstExpr, FunctionBody, HeapType, Operator, ValType};

use crate::parse::{ModuleIr, Signature, TypeEntry};
use crate::{TranspileError, codegen};

/// How deep a chain of imports answered by imports may go before it is called a cycle.
const MAX_IMPORT_CHAIN: usize = 64;

/// What provides an import of a linked module. Indices are in the providing module's own
/// index space (an import of it is followed in turn).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Source {
    /// Not provided by a linked module: the host's. Only the main module may keep one.
    Host,
    Func {
        module: usize,
        index: u32,
    },
    Global {
        module: usize,
        index: u32,
    },
    Memory {
        module: usize,
        index: u32,
    },
    Table {
        module: usize,
        index: u32,
    },
}

impl Source {
    /// The providing module and index, when this source is an item of `space`.
    fn provider(&self, space: Space) -> Option<(usize, u32)> {
        match (self, space) {
            (Self::Func { module, index }, Space::Func)
            | (Self::Global { module, index }, Space::Global)
            | (Self::Table { module, index }, Space::Table)
            | (Self::Memory { module, index }, Space::Memory) => Some((*module, *index)),
            _ => None,
        }
    }
}

/// What provides each import of one module, in import order. A missing entry means
/// [`Source::Host`].
#[derive(Debug, Default, Clone)]
pub(crate) struct ImportSources {
    pub funcs: Vec<Source>,
    pub globals: Vec<Source>,
    pub memory: Option<Source>,
    pub tables: Vec<Source>,
}

/// A parsed core module and where its imports come from.
pub(crate) struct LinkModule<'a> {
    pub ir: ModuleIr<'a>,
    pub sources: ImportSources,
    /// Function exports of an appended module that the linked module exports too, as
    /// `(its export name, the name in the linked module)`. Other exports of appended modules
    /// are dropped. Ignored for the main module, which keeps all of its exports.
    pub carried: Vec<(String, String)>,
}

/// The bytes the linker writes: rewritten bodies of the appended modules and the body of a
/// synthesized start function. Owned by the caller so the linked [`ModuleIr`] can borrow them.
#[derive(Debug, Default)]
pub(crate) struct Rewritten {
    /// Per appended module (the main module excluded), per defined function.
    bodies: Vec<Vec<Vec<u8>>>,
    start_body: Option<Vec<u8>>,
}

fn unsupported(what: impl Into<String>) -> TranspileError {
    TranspileError::Unsupported(what.into())
}

fn to_u32(n: usize) -> Result<u32, TranspileError> {
    u32::try_from(n).map_err(|_| unsupported("linked module is too large"))
}

fn add(a: u32, b: u32) -> Result<u32, TranspileError> {
    a.checked_add(b)
        .ok_or_else(|| unsupported("linked module is too large"))
}

/// An index space an import can name.
#[derive(Debug, Clone, Copy)]
enum Space {
    Func,
    Global,
    Table,
    Memory,
}

impl Space {
    fn name(self) -> &'static str {
        match self {
            Self::Func => "function",
            Self::Global => "global",
            Self::Table => "table",
            Self::Memory => "memory",
        }
    }
}

/// One count (or offset) per importable index space.
#[derive(Debug, Default, Clone, Copy)]
struct PerSpace {
    func: u32,
    global: u32,
    table: u32,
    memory: u32,
}

impl PerSpace {
    fn get(self, space: Space) -> u32 {
        match space {
            Space::Func => self.func,
            Space::Global => self.global,
            Space::Table => self.table,
            Space::Memory => self.memory,
        }
    }

    fn add(self, other: Self) -> Result<Self, TranspileError> {
        Ok(Self {
            func: add(self.func, other.func)?,
            global: add(self.global, other.global)?,
            table: add(self.table, other.table)?,
            memory: add(self.memory, other.memory)?,
        })
    }
}

/// Where a module's items start in every merged index space (or, as a running total, how
/// many items the merged spaces hold).
#[derive(Debug, Default, Clone, Copy)]
struct Offsets {
    spaces: PerSpace,
    types: u32,
    data: u32,
    elems: u32,
}

/// The sizes and wiring of one module, copied out so the layout does not borrow the IR.
struct ModInfo {
    sources: ImportSources,
    /// `(module, field)` of each function import, for error messages.
    func_names: Vec<(String, String)>,
    imports: PerSpace,
    defined: PerSpace,
    types: u32,
    data: u32,
    elems: u32,
    /// Filled in by [`Layout::new`]; zero everywhere for the main module.
    offsets: Offsets,
    has_start: bool,
    /// Per function import: the merged index of its tail slot when the host answers it
    /// (appended modules only; the main module's host imports keep their own slots).
    tail_slots: Vec<Option<u32>>,
}

impl ModInfo {
    fn of(module: &LinkModule<'_>) -> Result<Self, TranspileError> {
        let ir = &module.ir;
        let tables: Vec<&codegen::TableInfo> =
            ir.table.iter().chain(ir.extra_tables.iter()).collect();
        let table_imports = tables.iter().filter(|t| t.imported).count();
        Ok(Self {
            sources: module.sources.clone(),
            func_names: ir
                .imports
                .iter()
                .map(|im| (im.module.clone(), im.field.clone()))
                .collect(),
            imports: PerSpace {
                func: to_u32(ir.imports.len())?,
                global: to_u32(ir.imported_globals.len())?,
                table: to_u32(table_imports)?,
                memory: to_u32(ir.memory_imports)?,
            },
            defined: PerSpace {
                func: to_u32(ir.bodies.len())?,
                global: to_u32(ir.globals.len())?,
                table: to_u32(tables.len().saturating_sub(table_imports))?,
                memory: to_u32(ir.memories.len().saturating_sub(ir.memory_imports))?,
            },
            types: to_u32(ir.types.kinds.len())?,
            data: to_u32(ir.data.len())?,
            elems: to_u32(ir.elements.len())?,
            offsets: Offsets::default(),
            has_start: ir.start.is_some(),
            tail_slots: Vec::new(),
        })
    }

    /// What provides import `j` of `space`.
    fn source(&self, space: Space, j: u32) -> &Source {
        let list = match space {
            Space::Func => &self.sources.funcs,
            Space::Global => &self.sources.globals,
            Space::Table => &self.sources.tables,
            Space::Memory => return self.sources.memory.as_ref().unwrap_or(&Source::Host),
        };
        usize::try_from(j)
            .ok()
            .and_then(|j| list.get(j))
            .unwrap_or(&Source::Host)
    }

    /// How an import of `space` is named in an error message.
    fn import_name(&self, space: Space, j: u32) -> String {
        match space {
            Space::Func => {
                let (module, field) = usize::try_from(j)
                    .ok()
                    .and_then(|j| self.func_names.get(j))
                    .map_or(("?", "?"), |(a, b)| (a.as_str(), b.as_str()));
                format!("`{module}.{field}`")
            }
            Space::Global => format!("(global {j})"),
            Space::Table => format!("(table {j})"),
            Space::Memory => "(memory)".to_owned(),
        }
    }
}

/// The offset of each appended module in every merged index space, and the resolution of
/// imports. The main module (index 0) has offset 0 everywhere.
struct Layout {
    mods: Vec<ModInfo>,
    /// Total merged function count (the index a synthesized function takes).
    total_funcs: u32,
}

impl Layout {
    fn new(modules: &[LinkModule<'_>]) -> Result<Self, TranspileError> {
        let mut mods = modules
            .iter()
            .map(ModInfo::of)
            .collect::<Result<Vec<_>, _>>()?;
        // The running size of each merged space: the main module's, then each appended
        // module's defined items.
        let mut next = Offsets::default();
        for (m, info) in mods.iter_mut().enumerate() {
            let main = m == 0;
            if !main {
                info.offsets = next;
            }
            // The main module contributes its imports too; an appended one only what it defines.
            let mut spaces = next.spaces.add(info.defined)?;
            if main {
                spaces = spaces.add(info.imports)?;
            }
            next = Offsets {
                spaces,
                types: add(next.types, info.types)?,
                data: add(next.data, info.data)?,
                elems: add(next.elems, info.elems)?,
            };
        }
        // Host imports of appended modules go after the defined functions, and after the
        // function that starts the several modules when one is synthesized.
        let synthesized_start = mods.iter().filter(|i| i.has_start).count() > 1;
        let mut tail_next = add(next.spaces.func, u32::from(synthesized_start))?;
        for info in mods.iter_mut().skip(1) {
            let mut slots = Vec::new();
            for j in 0..info.imports.func {
                if *info.source(Space::Func, j) == Source::Host {
                    slots.push(Some(tail_next));
                    tail_next = add(tail_next, 1)?;
                } else {
                    slots.push(None);
                }
            }
            info.tail_slots = slots;
        }
        let layout = Self {
            mods,
            total_funcs: next.spaces.func,
        };
        layout.check_imports()?;
        Ok(layout)
    }

    /// The host imports of the appended modules, in tail-slot order.
    fn tail_imports(
        &self,
        appended: &[(usize, ModuleIr<'_>)],
    ) -> Result<Vec<codegen::ImportInfo>, TranspileError> {
        let mut tail = Vec::new();
        for (m, ir) in appended {
            let slots = &self.info(*m)?.tail_slots;
            for (im, slot) in ir.imports.iter().zip(slots) {
                if slot.is_some() {
                    tail.push(codegen::ImportInfo {
                        type_index: self.type_(*m, im.type_index)?,
                        params: im.params.clone(),
                        results: im.results.clone(),
                        wasi: None,
                        module: im.module.clone(),
                        field: im.field.clone(),
                        ident: None,
                        alias: None,
                    });
                }
            }
        }
        Ok(tail)
    }

    fn info(&self, m: usize) -> Result<&ModInfo, TranspileError> {
        self.mods
            .get(m)
            .ok_or_else(|| unsupported(format!("import from the unknown linked module {m}")))
    }

    /// Fail early on an import that cannot be bound: a host import of an appended module,
    /// an import of the main module (other than a function) that another module provides,
    /// or a chain of imports that never ends.
    fn check_imports(&self) -> Result<(), TranspileError> {
        for (m, info) in self.mods.iter().enumerate() {
            for j in 0..info.imports.func {
                self.func(m, j)?;
            }
            if m == 0 {
                let moved = info
                    .sources
                    .globals
                    .iter()
                    .chain(info.sources.memory.iter())
                    .chain(info.sources.tables.iter())
                    .any(|s| *s != Source::Host);
                if moved {
                    return Err(unsupported(
                        "the main module imports a global, memory or table that another module provides",
                    ));
                }
                continue;
            }
            for space in [Space::Global, Space::Table, Space::Memory] {
                for j in 0..info.imports.get(space) {
                    self.resolve(space, m, j)?;
                }
            }
        }
        Ok(())
    }

    /// The merged index of item `j` of `space` in module `m`, following imports to the
    /// module that defines the item.
    fn resolve(&self, space: Space, mut m: usize, mut j: u32) -> Result<u32, TranspileError> {
        for _ in 0..=MAX_IMPORT_CHAIN {
            let info = self.info(m)?;
            let Some(defined) = j.checked_sub(info.imports.get(space)) else {
                let source = info.source(space, j);
                if let Some((module, index)) = source.provider(space) {
                    (m, j) = (module, index);
                    continue;
                }
                return match source {
                    Source::Host if m == 0 => Ok(j),
                    Source::Host => match space {
                        Space::Func => usize::try_from(j)
                            .ok()
                            .and_then(|j| info.tail_slots.get(j).copied().flatten())
                            .ok_or_else(|| {
                                TranspileError::Internal("host import without a tail slot".into())
                            }),
                        _ => Err(unsupported(format!(
                            "host import {} of linked module {m}: only the main module can import a global, memory or table from the host",
                            info.import_name(space, j)
                        ))),
                    },
                    other => Err(unsupported(format!(
                        "{} import of linked module {m} is provided by {other:?}",
                        space.name()
                    ))),
                };
            };
            if m == 0 {
                return Ok(j);
            }
            return add(info.offsets.spaces.get(space), defined);
        }
        Err(unsupported(format!(
            "{} imports of linked modules form a cycle",
            space.name()
        )))
    }

    fn func(&self, m: usize, j: u32) -> Result<u32, TranspileError> {
        self.resolve(Space::Func, m, j)
    }

    fn global(&self, m: usize, j: u32) -> Result<u32, TranspileError> {
        self.resolve(Space::Global, m, j)
    }

    fn table(&self, m: usize, j: u32) -> Result<u32, TranspileError> {
        self.resolve(Space::Table, m, j)
    }

    fn memory(&self, m: usize, j: u32) -> Result<u32, TranspileError> {
        self.resolve(Space::Memory, m, j)
    }

    fn type_(&self, m: usize, t: u32) -> Result<u32, TranspileError> {
        add(self.info(m)?.offsets.types, t)
    }

    fn data(&self, m: usize, d: u32) -> Result<u32, TranspileError> {
        add(self.info(m)?.offsets.data, d)
    }

    fn elem(&self, m: usize, e: u32) -> Result<u32, TranspileError> {
        add(self.info(m)?.offsets.elems, e)
    }

    /// The merged function indices of every start function, in run order: main's, then the
    /// appended modules' in the order given.
    fn start_funcs(&self, modules: &[LinkModule<'_>]) -> Result<Vec<u32>, TranspileError> {
        modules
            .iter()
            .enumerate()
            .filter_map(|(m, module)| module.ir.start.map(|s| self.func(m, s)))
            .collect()
    }
}

// --- operator classification -------------------------------------------------------------

/// The proposal an operator belongs to and its `visit_*` name (the wasm text name, with
/// underscores), for the operators outside the subset an appended module may use.
fn operator_origin(op: &Operator<'_>) -> (&'static str, &'static str) {
    macro_rules! origin {
        ($( @$proposal:ident $op:ident $({ $($arg:ident: $argty:ty),* })? => $visit:ident ($($ann:tt)*))*) => {
            match op {
                $(
                    Operator::$op $({ $($arg: _),* })? => (
                        stringify!($proposal),
                        stringify!($visit).trim_start_matches("visit_"),
                    ),
                )*
                _ => ("unknown", "unknown"),
            }
        };
    }
    wasmparser::for_each_operator!(origin)
}

/// Proposals an appended module's bodies may use (decision Q6).
fn allowed_proposal(proposal: &str) -> bool {
    matches!(
        proposal,
        "mvp" | "sign_extension" | "bulk_memory" | "reference_types" | "saturating_float_to_int"
    )
}

/// Reject a typed reference where the linker copies a value type verbatim (function types,
/// globals). Body types are checked by [`Remap::heap_type`] instead.
fn check_val_type(ty: ValType, what: &str) -> Result<(), TranspileError> {
    match ty {
        ValType::Ref(r) if matches!(r.heap_type(), HeapType::Concrete(_)) => Err(unsupported(
            format!("typed reference in {what} of a linked module"),
        )),
        _ => Ok(()),
    }
}

// --- re-encoding -------------------------------------------------------------------------

/// Re-encodes the operators of appended module `module` with the merged index spaces. Every
/// index immediate goes through the [`Reencode`] hooks, so an operator naming an index
/// needs no hand-written opcode here.
struct Remap<'l> {
    layout: &'l Layout,
    module: usize,
}

type ReencodeResult<T> = Result<T, reencode::Error<TranspileError>>;

fn user_error<T>(e: TranspileError) -> ReencodeResult<T> {
    Err(reencode::Error::UserError(e))
}

/// A [`Reencode`] failure as the error the linker reports.
fn from_reencode(e: reencode::Error<TranspileError>) -> TranspileError {
    match e {
        reencode::Error::UserError(e) => e,
        reencode::Error::ParseError(e) => TranspileError::Parse(e),
        other => TranspileError::Internal(format!("re-encoding a linked body: {other}")),
    }
}

impl Reencode for Remap<'_> {
    type Error = TranspileError;

    fn function_index(&mut self, i: u32) -> ReencodeResult<u32> {
        self.layout.func(self.module, i).or_else(user_error)
    }

    fn global_index(&mut self, i: u32) -> ReencodeResult<u32> {
        self.layout.global(self.module, i).or_else(user_error)
    }

    fn table_index(&mut self, i: u32) -> ReencodeResult<u32> {
        self.layout.table(self.module, i).or_else(user_error)
    }

    fn memory_index(&mut self, i: u32) -> ReencodeResult<u32> {
        self.layout.memory(self.module, i).or_else(user_error)
    }

    fn type_index(&mut self, i: u32) -> ReencodeResult<u32> {
        self.layout.type_(self.module, i).or_else(user_error)
    }

    fn data_index(&mut self, i: u32) -> ReencodeResult<u32> {
        self.layout.data(self.module, i).or_else(user_error)
    }

    fn element_index(&mut self, i: u32) -> ReencodeResult<u32> {
        self.layout.elem(self.module, i).or_else(user_error)
    }

    // A concrete heap type would need a type index of the merged type space that the
    // linker does not model for references.
    fn heap_type(&mut self, ty: HeapType) -> ReencodeResult<wasm_encoder::HeapType> {
        if matches!(ty, HeapType::Concrete(_)) {
            return user_error(unsupported("typed reference in a linked module"));
        }
        reencode::utils::heap_type(self, ty)
    }

    fn mem_arg(&mut self, arg: wasmparser::MemArg) -> ReencodeResult<wasm_encoder::MemArg> {
        if matches!(arg.target, wasmparser::MemArgTarget::Array(_)) {
            return user_error(unsupported("GC array access in a linked module"));
        }
        reencode::utils::mem_arg(self, arg)
    }

    fn instruction<'a>(
        &mut self,
        op: Operator<'a>,
    ) -> ReencodeResult<wasm_encoder::Instruction<'a>> {
        let (proposal, name) = operator_origin(&op);
        if !allowed_proposal(proposal) {
            return user_error(unsupported(format!(
                "operator `{name}` ({proposal}) in a linked module: only MVP, bulk-memory, sign-extension and reference-types operators can be linked"
            )));
        }
        reencode::utils::instruction(self, op)
    }
}

/// Rewrite one body of appended module `m`: the locals are copied, every operator is
/// re-encoded with the merged indices.
fn rewrite_body(
    layout: &Layout,
    m: usize,
    body: &FunctionBody<'_>,
) -> Result<Vec<u8>, TranspileError> {
    let mut remap = Remap { layout, module: m };
    let mut func = remap
        .new_function_with_parsed_locals(body)
        .map_err(from_reencode)?;
    let mut ops = body.get_operators_reader()?;
    while !ops.eof() {
        let op = remap.parse_instruction(&mut ops).map_err(from_reencode)?;
        func.instruction(&op);
    }
    Ok(func.into_raw_body())
}

/// Re-encode the initializer of a defined global of module `m`. Only the constant forms a
/// global initializer takes are accepted.
fn rewrite_const(
    layout: &Layout,
    m: usize,
    expr: &ConstExpr<'_>,
) -> Result<Vec<u8>, TranspileError> {
    let mut remap = Remap { layout, module: m };
    let mut out = Vec::new();
    for op in expr.get_operators_reader() {
        let op = op?;
        match op {
            Operator::GlobalGet { .. }
            | Operator::RefFunc { .. }
            | Operator::RefNull {
                hty: HeapType::Abstract { .. },
            }
            | Operator::I32Const { .. }
            | Operator::I64Const { .. }
            | Operator::F32Const { .. }
            | Operator::F64Const { .. }
            | Operator::I32Add
            | Operator::I32Sub
            | Operator::I32Mul
            | Operator::I64Add
            | Operator::I64Sub
            | Operator::I64Mul
            | Operator::End => remap
                .instruction(op)
                .map_err(from_reencode)?
                .encode(&mut out),
            other => {
                let (_, name) = operator_origin(&other);
                return Err(unsupported(format!(
                    "operator `{name}` in a global initializer of a linked module"
                )));
            }
        }
    }
    Ok(out)
}

/// The body of a function that calls each of `targets` in order.
fn start_body(targets: &[u32]) -> Vec<u8> {
    let mut func = wasm_encoder::Function::new([]);
    for &t in targets {
        func.instructions().call(t);
    }
    func.instructions().end();
    func.into_raw_body()
}

/// Rewrite the bodies of every appended module (and build the synthesized start function when
/// several modules start). Must be called with the modules later given to [`link`].
pub(crate) fn rewrite_bodies(modules: &[LinkModule<'_>]) -> Result<Rewritten, TranspileError> {
    if modules.len() < 2 {
        return Ok(Rewritten::default());
    }
    let layout = Layout::new(modules)?;
    let mut bodies = Vec::with_capacity(modules.len().saturating_sub(1));
    for (m, module) in modules.iter().enumerate().skip(1) {
        check_appended(&module.ir, m)?;
        bodies.push(
            module
                .ir
                .bodies
                .iter()
                .map(|b| rewrite_body(&layout, m, b))
                .collect::<Result<Vec<_>, _>>()?,
        );
    }
    let starts = layout.start_funcs(modules)?;
    let start_body = (starts.len() > 1).then(|| start_body(&starts));
    Ok(Rewritten { bodies, start_body })
}

/// Reject what an appended module may not contain regardless of its bodies.
fn check_appended(ir: &ModuleIr<'_>, m: usize) -> Result<(), TranspileError> {
    if !ir.tags.is_empty() {
        return Err(unsupported(format!("exception tag in linked module {m}")));
    }
    for (kind, sig) in ir.types.kinds.iter().zip(&ir.types.signatures) {
        if !matches!(kind, codegen::CompositeKind::Func) {
            return Err(unsupported(format!(
                "a GC or continuation type in linked module {m}"
            )));
        }
        for ty in sig.params.iter().chain(&sig.results) {
            check_val_type(*ty, "a function type")?;
        }
    }
    for g in &ir.globals {
        check_val_type(g.ty, "a global")?;
    }
    Ok(())
}

fn copy_table(t: &codegen::TableInfo, init: Option<codegen::TableFill>) -> codegen::TableInfo {
    codegen::TableInfo {
        min: t.min,
        max: t.max,
        imported: t.imported,
        table64: t.table64,
        element: t.element,
        init,
    }
}

/// An appended module's segment offset in the merged module. An offset that reads an
/// imported global (`__table_base`, `__memory_base`) is folded to a constant when the global
/// resolves to an immutable integer constant of another module, which is how a dynamic
/// library is placed by the program that loads it; any other such offset is refused.
fn merged_offset(
    layout: &Layout,
    ir: &ModuleIr<'_>,
    m: usize,
    offset: &Option<codegen::SegOffset>,
    what: &str,
) -> Result<Option<codegen::SegOffset>, TranspileError> {
    match offset {
        None => Ok(None),
        Some(codegen::SegOffset::Const(v)) => Ok(Some(codegen::SegOffset::Const(*v))),
        Some(codegen::SegOffset::Runtime { expr, .. }) => {
            let bytes = rewrite_const(layout, m, &ConstExpr::new(BinaryReader::new(expr, 0)))?;
            let values = codegen::IntGlobals {
                imported: ir.imported_globals.len(),
                values: &ir.global_ints,
            };
            let value = codegen::const_expr_u64_for(
                &ConstExpr::new(BinaryReader::new(&bytes, 0)),
                what,
                values,
            )
            .map_err(|_| {
                unsupported(format!(
                    "{what} offset of linked module {m} reads a global that is not an immutable \
                     integer constant of the module that provides it"
                ))
            })?;
            Ok(Some(codegen::SegOffset::Const(value)))
        }
    }
}

/// Link `modules` (the main module first) into one module. `rewritten` is what
/// [`rewrite_bodies`] returned for the same modules; the result borrows its bytes.
///
/// A single module is returned unchanged.
pub(crate) fn link<'b>(
    modules: Vec<LinkModule<'b>>,
    rewritten: &'b Rewritten,
) -> Result<ModuleIr<'b>, TranspileError> {
    if modules.len() < 2 {
        return modules
            .into_iter()
            .next()
            .map(|m| m.ir)
            .ok_or_else(|| unsupported("no module to link"));
    }
    let layout = Layout::new(&modules)?;
    let starts = layout.start_funcs(&modules)?;
    let carried: Vec<Vec<(String, String)>> = modules.iter().map(|m| m.carried.clone()).collect();
    let mut modules = modules.into_iter();
    let mut ir = modules
        .next()
        .ok_or_else(|| unsupported("no module to link"))?
        .ir;
    let appended: Vec<(usize, ModuleIr<'b>)> =
        modules.enumerate().map(|(i, m)| (i + 1, m.ir)).collect();

    // A main import another module answers keeps its slot and becomes a direct call.
    let main_sources = &layout.info(0)?.sources.funcs;
    for (j, im) in ir.imports.iter_mut().enumerate() {
        if main_sources.get(j).is_some_and(|s| *s != Source::Host) {
            im.alias = Some(layout.func(0, to_u32(j)?)?);
            im.wasi = None;
        }
    }

    let mut by_sig = seed_canon(&ir)?;
    for (m, a) in &appended {
        check_appended(a, *m)?;
        merge_types(&layout, &mut ir, &mut by_sig, *m, a)?;
    }
    merge_functions(&layout, &mut ir, &appended, rewritten)?;
    ir.tail_imports = layout.tail_imports(&appended)?;
    carry_exports(&layout, &mut ir, &appended, &carried)?;
    merge_globals(&layout, &mut ir, &appended)?;
    merge_memories(&mut ir, &appended);
    merge_tables(&layout, &mut ir, &appended)?;
    merge_segments(&layout, &mut ir, &appended)?;

    ir.start = match (starts.as_slice(), &rewritten.start_body) {
        ([], _) => None,
        ([only], _) => Some(*only),
        (_, Some(body)) => {
            let unit = unit_function_type(&mut ir)?;
            ir.func_type_indices.push(unit);
            ir.bodies
                .push(FunctionBody::new(BinaryReader::new(body, 0)));
            Some(layout.total_funcs)
        }
        (_, None) => {
            return Err(TranspileError::Internal(
                "several start functions without a synthesized start".into(),
            ));
        }
    };
    ir.global_inits = Vec::new();
    Ok(ir)
}

/// Append a function type with canonical id `canon` to every view of the type space.
fn push_func_type(
    ir: &mut ModuleIr<'_>,
    params: &[ValType],
    results: &[ValType],
    super_: Option<u32>,
    canon: u32,
) {
    ir.type_rel.canon.push(canon);
    ir.type_sigs.push(codegen::TypeSig {
        params: params.to_vec(),
        results: results.to_vec(),
    });
    ir.types.push(TypeEntry {
        kind: codegen::CompositeKind::Func,
        sig: Signature {
            params: params.to_vec(),
            results: results.to_vec(),
        },
        super_,
    });
}

/// The index of a `() -> ()` function type, added when the module has none.
fn unit_function_type(ir: &mut ModuleIr<'_>) -> Result<u32, TranspileError> {
    let found = ir
        .types
        .kinds
        .iter()
        .zip(&ir.types.signatures)
        .position(|(k, s)| {
            matches!(k, codegen::CompositeKind::Func) && s.params.is_empty() && s.results.is_empty()
        });
    if let Some(at) = found {
        return to_u32(at);
    }
    let at = to_u32(ir.types.kinds.len())?;
    let canon = to_u32(ir.type_rel.canon_super.len())?;
    ir.type_rel.canon_super.push(None);
    push_func_type(ir, &[], &[], None, canon);
    Ok(at)
}

/// Append the types of module `m`. Function types with equal signatures share a canonical
/// id across modules, so a `call_indirect` of one module can reach another's function.
/// The canonical id of each function signature, shared by every appended module so that a
/// `call_indirect` in one reaches a function declared in another (see [`merge_types`]).
type CanonBySig = HashMap<(Vec<ValType>, Vec<ValType>), u32>;

/// Seeds the canonical ids from the types the main module's functions and imports are declared
/// with: those are the ids its `call_indirect` targets are compared against (the first type of
/// a given shape may sit in a rec group or have a supertype, and is not).
fn seed_canon(ir: &ModuleIr<'_>) -> Result<CanonBySig, TranspileError> {
    let mut by_sig = CanonBySig::new();
    let declared = ir
        .func_type_indices
        .iter()
        .copied()
        .chain(ir.imports.iter().map(|im| im.type_index));
    for t in declared {
        let at = usize::try_from(t).map_err(|_| unsupported("type index too large"))?;
        if let (Some(sig), Some(canon)) = (ir.types.signatures.get(at), ir.type_rel.canon.get(at)) {
            by_sig
                .entry((sig.params.clone(), sig.results.clone()))
                .or_insert(*canon);
        }
    }
    Ok(by_sig)
}

fn merge_types(
    layout: &Layout,
    ir: &mut ModuleIr<'_>,
    by_sig: &mut CanonBySig,
    m: usize,
    appended: &ModuleIr<'_>,
) -> Result<(), TranspileError> {
    for (k, sig) in appended.types.signatures.iter().enumerate() {
        let super_ = appended
            .types
            .supers
            .get(k)
            .copied()
            .flatten()
            .map(|s| layout.type_(m, s))
            .transpose()?;
        let canon = match super_ {
            None => {
                let key = (sig.params.clone(), sig.results.clone());
                match by_sig.get(&key) {
                    Some(c) => *c,
                    None => {
                        let fresh = to_u32(ir.type_rel.canon_super.len())?;
                        ir.type_rel.canon_super.push(None);
                        by_sig.insert(key, fresh);
                        fresh
                    }
                }
            }
            Some(s) => {
                let parent = usize::try_from(s)
                    .ok()
                    .and_then(|s| ir.type_rel.canon.get(s))
                    .copied()
                    .ok_or_else(|| TranspileError::Internal("supertype out of range".into()))?;
                let fresh = to_u32(ir.type_rel.canon_super.len())?;
                ir.type_rel.canon_super.push(Some(parent));
                fresh
            }
        };
        push_func_type(ir, &sig.params, &sig.results, super_, canon);
    }
    Ok(())
}

/// Append the functions of every appended module: their types, rewritten bodies and the
/// functions their initializers name.
fn merge_functions<'b>(
    layout: &Layout,
    ir: &mut ModuleIr<'b>,
    appended: &[(usize, ModuleIr<'b>)],
    rewritten: &'b Rewritten,
) -> Result<(), TranspileError> {
    for (m, a) in appended {
        let m = *m;
        for t in &a.func_type_indices {
            ir.func_type_indices.push(layout.type_(m, *t)?);
        }
        let bodies = m
            .checked_sub(1)
            .and_then(|i| rewritten.bodies.get(i))
            .ok_or_else(|| TranspileError::Internal("bodies were not rewritten".into()))?;
        for body in bodies {
            ir.bodies
                .push(FunctionBody::new(BinaryReader::new(body, 0)));
        }
        for g in &a.global_funcs {
            ir.global_funcs.push(layout.func(m, *g)?);
        }
    }
    Ok(())
}

/// Adds the function exports the caller asked to keep from the appended modules.
fn carry_exports(
    layout: &Layout,
    ir: &mut ModuleIr<'_>,
    appended: &[(usize, ModuleIr<'_>)],
    carried: &[Vec<(String, String)>],
) -> Result<(), TranspileError> {
    for (m, a) in appended {
        for (own, name) in carried.get(*m).into_iter().flatten() {
            let export = a.exports.iter().find(|e| e.name == *own).ok_or_else(|| {
                unsupported(format!(
                    "linked module {m} does not export function `{own}`"
                ))
            })?;
            if ir.exports.iter().any(|e| e.name == *name) {
                return Err(unsupported(format!(
                    "linked modules export `{name}` more than once"
                )));
            }
            ir.exports.push(codegen::ExportInfo {
                name: name.clone(),
                func_index: layout.func(*m, export.func_index)?,
            });
        }
    }
    Ok(())
}

fn merge_globals(
    layout: &Layout,
    ir: &mut ModuleIr<'_>,
    appended: &[(usize, ModuleIr<'_>)],
) -> Result<(), TranspileError> {
    for (m, a) in appended {
        for (g, info) in a.globals.iter().enumerate() {
            let expr = a
                .global_inits
                .get(g)
                .ok_or_else(|| TranspileError::Internal("global initializer missing".into()))?;
            let bytes = rewrite_const(layout, *m, expr)?;
            let init = codegen::const_expr_to_rust(
                &ConstExpr::new(BinaryReader::new(&bytes, 0)),
                &ir.types.kinds,
                &ir.imported_globals,
                &ir.globals,
            )?;
            let value = codegen::const_int_value(
                &ConstExpr::new(BinaryReader::new(&bytes, 0)),
                codegen::IntGlobals {
                    imported: ir.imported_globals.len(),
                    values: &ir.global_ints,
                },
            )
            .filter(|_| !info.mutable);
            ir.global_ints.push(value);
            ir.globals.push(codegen::GlobalInfo {
                ty: info.ty,
                mutable: info.mutable,
                init,
            });
        }
    }
    Ok(())
}

fn merge_memories(ir: &mut ModuleIr<'_>, appended: &[(usize, ModuleIr<'_>)]) {
    for (_, a) in appended {
        for mem in a.memories.iter().skip(a.memory_imports) {
            ir.memories.push(codegen::MemInfo {
                min_pages: mem.min_pages,
                max_pages: mem.max_pages,
                // Only the first memory of the merged module can be host-lent.
                imported: mem.imported && ir.memories.is_empty(),
                shared: mem.shared,
                memory64: mem.memory64,
            });
        }
    }
}

fn merge_tables(
    layout: &Layout,
    ir: &mut ModuleIr<'_>,
    appended: &[(usize, ModuleIr<'_>)],
) -> Result<(), TranspileError> {
    let mut tables: Vec<codegen::TableInfo> = ir.table.take().into_iter().collect();
    tables.append(&mut ir.extra_tables);
    for (m, a) in appended {
        for t in a
            .table
            .iter()
            .chain(a.extra_tables.iter())
            .filter(|t| !t.imported)
        {
            let init = match t.init {
                None => None,
                Some(codegen::TableFill::Func(f)) => {
                    Some(codegen::TableFill::Func(layout.func(*m, f)?))
                }
                Some(codegen::TableFill::ImportedGlobal(_)) => {
                    return Err(unsupported(format!(
                        "table initializer of linked module {m} that reads an imported global"
                    )));
                }
            };
            tables.push(copy_table(t, init));
        }
    }
    let mut tables = tables.into_iter();
    ir.table = tables.next();
    ir.extra_tables = tables.collect();
    Ok(())
}

fn merge_segments(
    layout: &Layout,
    ir: &mut ModuleIr<'_>,
    appended: &[(usize, ModuleIr<'_>)],
) -> Result<(), TranspileError> {
    for (m, a) in appended {
        let m = *m;
        for seg in &a.elements {
            if !seg.imported_items.is_empty() {
                return Err(unsupported(format!(
                    "element segment of linked module {m} that reads an imported global"
                )));
            }
            // `u32::MAX` marks a null entry, which names no function.
            let funcs = seg
                .funcs
                .iter()
                .map(|&f| {
                    if f == u32::MAX {
                        Ok(f)
                    } else {
                        layout.func(m, f)
                    }
                })
                .collect::<Result<Vec<_>, _>>()?;
            let table = if seg.offset.is_some() {
                layout.table(m, seg.table)?
            } else {
                0
            };
            ir.elements.push(codegen::ElemSegment {
                table,
                offset: merged_offset(layout, ir, m, &seg.offset, "element segment")?,
                declared: seg.declared,
                funcs,
                imported_items: Vec::new(),
            });
        }
        for seg in &a.data {
            let mem_index = if seg.offset.is_some() {
                layout.memory(m, seg.mem_index)?
            } else {
                0
            };
            ir.data.push(codegen::DataSegment {
                offset: merged_offset(layout, ir, m, &seg.offset, "data segment")?,
                mem_index,
                bytes: seg.bytes.clone(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
