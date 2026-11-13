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

use wasmparser::{BinaryReader, BlockType, ConstExpr, FunctionBody, HeapType, Operator, ValType};

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

/// The opcode of a load/store (a `memarg` operator of the MVP).
fn memarg_opcode(op: &Operator<'_>) -> Option<(u8, wasmparser::MemArg)> {
    use Operator::*;
    Some(match op {
        I32Load { memarg } => (0x28, *memarg),
        I64Load { memarg } => (0x29, *memarg),
        F32Load { memarg } => (0x2A, *memarg),
        F64Load { memarg } => (0x2B, *memarg),
        I32Load8S { memarg } => (0x2C, *memarg),
        I32Load8U { memarg } => (0x2D, *memarg),
        I32Load16S { memarg } => (0x2E, *memarg),
        I32Load16U { memarg } => (0x2F, *memarg),
        I64Load8S { memarg } => (0x30, *memarg),
        I64Load8U { memarg } => (0x31, *memarg),
        I64Load16S { memarg } => (0x32, *memarg),
        I64Load16U { memarg } => (0x33, *memarg),
        I64Load32S { memarg } => (0x34, *memarg),
        I64Load32U { memarg } => (0x35, *memarg),
        I32Store { memarg } => (0x36, *memarg),
        I64Store { memarg } => (0x37, *memarg),
        F32Store { memarg } => (0x38, *memarg),
        F64Store { memarg } => (0x39, *memarg),
        I32Store8 { memarg } => (0x3A, *memarg),
        I32Store16 { memarg } => (0x3B, *memarg),
        I64Store8 { memarg } => (0x3C, *memarg),
        I64Store16 { memarg } => (0x3D, *memarg),
        I64Store32 { memarg } => (0x3E, *memarg),
        _ => return None,
    })
}

// --- encoding ----------------------------------------------------------------------------

/// An unsigned LEB128 (the same bytes for a `u32` and a `u64` of equal value).
fn write_uleb(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let byte = v.to_le_bytes()[0] & 0x7f;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn write_u32(out: &mut Vec<u8>, v: u32) {
    write_uleb(out, u64::from(v));
}

/// A non-negative value as a signed LEB128 (a block type's type index is an `s33`).
fn write_s33(out: &mut Vec<u8>, index: u32) {
    let mut v = i64::from(index);
    loop {
        let byte = v.to_le_bytes()[0] & 0x7f;
        v >>= 7;
        if v == 0 && byte & 0x40 == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// A single-byte operator followed by its index immediates.
fn encode(out: &mut Vec<u8>, opcode: u8, immediates: &[u32]) {
    out.push(opcode);
    for &i in immediates {
        write_u32(out, i);
    }
}

/// A `0xFC`-prefixed operator: the prefix, its sub-opcode, then the immediates.
fn prefixed(out: &mut Vec<u8>, sub: u32, immediates: &[u32]) {
    out.push(0xFC);
    write_u32(out, sub);
    for &i in immediates {
        write_u32(out, i);
    }
}

fn check_val_type(ty: ValType, what: &str) -> Result<(), TranspileError> {
    match ty {
        ValType::Ref(r) if matches!(r.heap_type(), HeapType::Concrete(_)) => Err(unsupported(
            format!("typed reference in {what} of a linked module"),
        )),
        _ => Ok(()),
    }
}

/// Re-encode a load/store of module `m` with the merged memory index.
fn rewrite_memarg(
    layout: &Layout,
    m: usize,
    opcode: u8,
    memarg: wasmparser::MemArg,
    out: &mut Vec<u8>,
) -> Result<(), TranspileError> {
    let source_memory = match memarg.target {
        wasmparser::MemArgTarget::Memory(i) => i,
        wasmparser::MemArgTarget::Array(_) => {
            return Err(unsupported("GC array access in a linked module"));
        }
    };
    let memory = layout.memory(m, source_memory)?;
    out.push(opcode);
    // Bit 6 of the alignment says an explicit memory index follows.
    if memory == 0 {
        write_u32(out, u32::from(memarg.align));
    } else {
        write_u32(out, u32::from(memarg.align) | 0x40);
        write_u32(out, memory);
    }
    write_uleb(out, memarg.offset);
    Ok(())
}

/// Re-encode a `block`/`loop`/`if` of module `m`: only a type-index block type names an index.
fn rewrite_block(
    layout: &Layout,
    m: usize,
    opcode: u8,
    blockty: BlockType,
    raw: &[u8],
    out: &mut Vec<u8>,
) -> Result<(), TranspileError> {
    match blockty {
        BlockType::FuncType(t) => {
            out.push(opcode);
            write_s33(out, layout.type_(m, t)?);
        }
        BlockType::Type(ty) => {
            check_val_type(ty, "a block type")?;
            out.extend_from_slice(raw);
        }
        BlockType::Empty => out.extend_from_slice(raw),
    }
    Ok(())
}

/// Re-encode one operator of module `m`, or copy its bytes when it names no index.
fn rewrite_operator(
    layout: &Layout,
    m: usize,
    op: &Operator<'_>,
    raw: &[u8],
    out: &mut Vec<u8>,
) -> Result<(), TranspileError> {
    use Operator::*;
    if let Some((opcode, memarg)) = memarg_opcode(op) {
        return rewrite_memarg(layout, m, opcode, memarg, out);
    }
    match op {
        Call { function_index } => encode(out, 0x10, &[layout.func(m, *function_index)?]),
        CallIndirect {
            type_index,
            table_index,
        } => encode(
            out,
            0x11,
            &[
                layout.type_(m, *type_index)?,
                layout.table(m, *table_index)?,
            ],
        ),
        RefFunc { function_index } => encode(out, 0xD2, &[layout.func(m, *function_index)?]),
        GlobalGet { global_index } => encode(out, 0x23, &[layout.global(m, *global_index)?]),
        GlobalSet { global_index } => encode(out, 0x24, &[layout.global(m, *global_index)?]),
        TableGet { table } => encode(out, 0x25, &[layout.table(m, *table)?]),
        TableSet { table } => encode(out, 0x26, &[layout.table(m, *table)?]),
        Block { blockty } => rewrite_block(layout, m, 0x02, *blockty, raw, out)?,
        Loop { blockty } => rewrite_block(layout, m, 0x03, *blockty, raw, out)?,
        If { blockty } => rewrite_block(layout, m, 0x04, *blockty, raw, out)?,
        MemorySize { mem } => encode(out, 0x3F, &[layout.memory(m, *mem)?]),
        MemoryGrow { mem } => encode(out, 0x40, &[layout.memory(m, *mem)?]),
        MemoryInit { data_index, mem } => prefixed(
            out,
            8,
            &[layout.data(m, *data_index)?, layout.memory(m, *mem)?],
        ),
        DataDrop { data_index } => prefixed(out, 9, &[layout.data(m, *data_index)?]),
        MemoryCopy { dst_mem, src_mem } => prefixed(
            out,
            10,
            &[layout.memory(m, *dst_mem)?, layout.memory(m, *src_mem)?],
        ),
        MemoryFill { mem } => prefixed(out, 11, &[layout.memory(m, *mem)?]),
        TableInit { elem_index, table } => prefixed(
            out,
            12,
            &[layout.elem(m, *elem_index)?, layout.table(m, *table)?],
        ),
        ElemDrop { elem_index } => prefixed(out, 13, &[layout.elem(m, *elem_index)?]),
        TableCopy {
            dst_table,
            src_table,
        } => prefixed(
            out,
            14,
            &[layout.table(m, *dst_table)?, layout.table(m, *src_table)?],
        ),
        TableGrow { table } => prefixed(out, 15, &[layout.table(m, *table)?]),
        TableSize { table } => prefixed(out, 16, &[layout.table(m, *table)?]),
        TableFill { table } => prefixed(out, 17, &[layout.table(m, *table)?]),
        RefNull { hty } => {
            if matches!(hty, HeapType::Concrete(_)) {
                return Err(unsupported("typed reference in a linked module"));
            }
            out.extend_from_slice(raw);
        }
        TypedSelect { ty } => {
            check_val_type(*ty, "a select")?;
            out.extend_from_slice(raw);
        }
        _ => {
            let (proposal, name) = operator_origin(op);
            if !allowed_proposal(proposal) {
                return Err(unsupported(format!(
                    "operator `{name}` ({proposal}) in a linked module: only MVP, bulk-memory, sign-extension and reference-types operators can be linked"
                )));
            }
            out.extend_from_slice(raw);
        }
    }
    Ok(())
}

/// The operators of `bytes` (whose first byte is at `base` in the original input), each with
/// the bytes it occupies.
fn operator_spans<'o, 'b>(
    bytes: &'b [u8],
    base: u64,
    ops: wasmparser::OperatorsIteratorWithOffsets<'o>,
) -> Result<Vec<(Operator<'o>, &'b [u8])>, TranspileError> {
    let ops: Vec<(Operator<'o>, u64)> = ops.collect::<Result<_, _>>()?;
    let starts = ops
        .iter()
        .map(|(_, at)| {
            at.checked_sub(base)
                .and_then(|r| usize::try_from(r).ok())
                .filter(|r| *r <= bytes.len())
                .ok_or_else(|| TranspileError::Internal("operator outside its body".into()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    // Each operator ends where the next one starts; the last one at the end of the bytes.
    let ends = starts
        .iter()
        .skip(1)
        .copied()
        .chain(std::iter::once(bytes.len()));
    ops.into_iter()
        .zip(starts.iter().copied().zip(ends))
        .map(|((op, _), (start, end))| {
            bytes
                .get(start..end)
                .map(|raw| (op, raw))
                .ok_or_else(|| TranspileError::Internal("operator span out of range".into()))
        })
        .collect()
}

/// Rewrite one body of appended module `m`: the locals are copied, every operator is
/// re-encoded with the merged indices.
fn rewrite_body(
    layout: &Layout,
    m: usize,
    body: &FunctionBody<'_>,
) -> Result<Vec<u8>, TranspileError> {
    let base = body.range().start;
    let bytes = body.as_bytes();
    let mut locals = body.get_locals_reader()?.into_iter();
    for local in &mut locals {
        check_val_type(local?.1, "a local")?;
    }
    let ops = locals.into_operators_reader();
    let ops_start = usize::try_from(
        ops.original_position()
            .checked_sub(base)
            .ok_or_else(|| TranspileError::Internal("body offset underflow".into()))?,
    )
    .map_err(|_| unsupported("module offset too large"))?;
    let mut out = bytes
        .get(..ops_start)
        .ok_or_else(|| TranspileError::Internal("locals outside their body".into()))?
        .to_vec();
    for (op, raw) in operator_spans(bytes, base, ops.into_iter_with_offsets())? {
        rewrite_operator(layout, m, &op, raw, &mut out)?;
    }
    Ok(out)
}

/// Re-encode the initializer of a defined global of module `m`. Only the constant forms a
/// global initializer takes are accepted.
fn rewrite_const(
    layout: &Layout,
    m: usize,
    expr: &ConstExpr<'_>,
) -> Result<Vec<u8>, TranspileError> {
    let mut reader = expr.get_binary_reader();
    let base = reader.original_position();
    let len = reader.bytes_remaining();
    let bytes = reader.read_bytes(len)?;
    let mut out = Vec::with_capacity(len);
    for (op, raw) in operator_spans(
        bytes,
        base,
        expr.get_operators_reader().into_iter_with_offsets(),
    )? {
        match op {
            Operator::GlobalGet { global_index } => {
                encode(&mut out, 0x23, &[layout.global(m, global_index)?]);
            }
            Operator::RefFunc { function_index } => {
                encode(&mut out, 0xD2, &[layout.func(m, function_index)?]);
            }
            Operator::RefNull {
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
            | Operator::End => out.extend_from_slice(raw),
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
    // No locals, then the calls, then `end`.
    let mut body = vec![0x00];
    for &t in targets {
        encode(&mut body, 0x10, &[t]);
    }
    body.push(0x0B);
    body
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
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]
mod tests {
    use super::*;
    use crate::{TranspileOptions, generate_from_ir, parse};
    use std::process::Command;

    fn wasm(wat: &str) -> Vec<u8> {
        wat::parse_str(wat).expect("valid wat")
    }

    fn func(module: usize, index: u32) -> Source {
        Source::Func { module, index }
    }

    /// Parse every module, link them and render the Rust source.
    fn linked_source(
        wasms: &[Vec<u8>],
        sources: Vec<ImportSources>,
    ) -> Result<String, TranspileError> {
        linked_source_with(wasms, sources, &TranspileOptions::default())
    }

    fn linked_source_with(
        wasms: &[Vec<u8>],
        sources: Vec<ImportSources>,
        topts: &TranspileOptions,
    ) -> Result<String, TranspileError> {
        let modules = wasms
            .iter()
            .zip(sources)
            .map(|(bytes, sources)| {
                Ok(LinkModule {
                    ir: parse::parse_module(bytes, topts)?,
                    sources,
                    carried: Vec::new(),
                })
            })
            .collect::<Result<Vec<_>, TranspileError>>()?;
        let rewritten = rewrite_bodies(&modules)?;
        let ir = link(modules, &rewritten)?;
        let mut code = None;
        generate_from_ir(ir, topts, |parts| {
            codegen::generate_module_split(parts, usize::MAX, usize::MAX, &mut |_, text| {
                code = Some(text);
                Ok(())
            })
        })?;
        code.ok_or_else(|| TranspileError::Internal("no source emitted".into()))
    }

    /// Compile the linked program with `main_body` appended and require it to exit cleanly.
    fn link_and_run(test: &str, wasms: &[Vec<u8>], sources: Vec<ImportSources>, main_body: &str) {
        let generated = linked_source(wasms, sources).expect("link and transpile");
        run_source(test, &generated, main_body);
    }

    fn run_source(test: &str, generated: &str, main_body: &str) {
        let dir = std::env::temp_dir().join(format!("wasm2rs_link_{test}_{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let src = dir.join("gen.rs");
        let bin = dir.join("gen");
        let program = format!("{generated}\nfn main() {{\n{main_body}\n}}\n");
        std::fs::write(&src, &program).expect("write source");
        let compile = Command::new("rustc")
            .current_dir(&dir)
            .args(["--edition", "2024", "-D", "warnings", "-C", "opt-level=0"])
            .arg(&src)
            .arg("-o")
            .arg(&bin)
            .output()
            .expect("run rustc");
        assert!(
            compile.status.success(),
            "linked code failed to compile:\n{program}\n{}",
            String::from_utf8_lossy(&compile.stderr)
        );
        let run = Command::new(&bin).status().expect("run binary");
        assert!(
            run.success(),
            "linked program assertions failed:\n{program}"
        );
    }

    fn link_error(wasms: &[Vec<u8>], sources: Vec<ImportSources>) -> String {
        match linked_source(wasms, sources) {
            Err(TranspileError::Unsupported(what)) => what,
            Err(other) => panic!("expected Unsupported, got {other}"),
            Ok(_) => panic!("expected the link to fail"),
        }
    }

    const MAIN_CALLS_B: &str = r#"(module
        (import "b" "g" (func $g (result i32)))
        (memory (export "memory") 1)
        (func (export "f") (result i32) i32.const 40)
        (func (export "_start") (result i32) call $g))"#;
    const B_USES_A: &str = r#"(module
        (import "a" "f" (func $f (result i32)))
        (import "a" "memory" (memory 1))
        (func (export "g") (result i32)
          i32.const 0 i32.const 2 i32.store
          call $f
          i32.const 0 i32.load
          i32.add))"#;

    fn two_module_sources() -> Vec<ImportSources> {
        vec![
            ImportSources {
                funcs: vec![func(1, 1)],
                ..ImportSources::default()
            },
            ImportSources {
                funcs: vec![func(0, 1)],
                memory: Some(Source::Memory {
                    module: 0,
                    index: 0,
                }),
                ..ImportSources::default()
            },
        ]
    }

    #[test]
    fn a_main_import_answered_by_another_module_is_a_direct_call_sharing_its_memory() {
        // main: imports b.g (func 0), defines f (1) and _start (2); B calls main's f and
        // reads the memory main owns.
        link_and_run(
            "two_modules",
            &[wasm(MAIN_CALLS_B), wasm(B_USES_A)],
            two_module_sources(),
            "let mut inst = Instance::new();\n    assert_eq!(inst.func2(), 42);",
        );
    }

    #[test]
    fn entry_exports_keep_the_function_an_aliased_import_reaches() {
        // `_start` is the only entry, and b's `g` is reachable only through main's alias.
        let topts = TranspileOptions::default().with_entry_exports(Some(vec!["_start".into()]));
        let generated = linked_source_with(
            &[wasm(MAIN_CALLS_B), wasm(B_USES_A)],
            two_module_sources(),
            &topts,
        )
        .expect("link and transpile");
        run_source(
            "entry_exports_alias",
            &generated,
            "let mut inst = Instance::new();\n    assert_eq!(inst.func2(), 42);",
        );
    }

    #[test]
    fn linking_one_module_changes_nothing() {
        let bytes = wasm(B_USES_A);
        let alone = linked_source(std::slice::from_ref(&bytes), vec![ImportSources::default()])
            .expect("link");
        assert_eq!(
            alone,
            crate::transpile_with_options(&bytes, &TranspileOptions::default()).expect("direct")
        );
    }

    #[test]
    fn a_table_filled_by_another_module_is_shared_with_the_shim_that_defines_it() {
        // The adapter shape: main calls a shim function that calls through the shim's table;
        // the fixup imports that table and fills it with a function of a third module.
        let main = r#"(module
            (import "shim" "0" (func $t (result i32)))
            (func (export "_start") (result i32) call $t))"#;
        let shim = r#"(module
            (type $r (func (result i32)))
            (table (export "imports") 1 funcref)
            (func (export "0") (result i32) i32.const 0 call_indirect (type $r)))"#;
        let adapter = r#"(module (func (export "answer") (result i32) i32.const 42))"#;
        let fixup = r#"(module
            (import "shim" "imports" (table 1 funcref))
            (import "adapter" "answer" (func $a (result i32)))
            (elem (i32.const 0) func $a))"#;
        let sources = vec![
            ImportSources {
                funcs: vec![func(1, 1)],
                ..ImportSources::default()
            },
            ImportSources::default(),
            ImportSources::default(),
            ImportSources {
                funcs: vec![func(2, 0)],
                tables: vec![Source::Table {
                    module: 1,
                    index: 0,
                }],
                ..ImportSources::default()
            },
        ];
        link_and_run(
            "shim_fixup",
            &[wasm(main), wasm(shim), wasm(adapter), wasm(fixup)],
            sources,
            "let mut inst = Instance::new();\n    assert_eq!(inst.func1(), 42);",
        );
    }

    #[test]
    fn an_appended_module_keeps_its_globals_segments_and_block_types() {
        let main = r#"(module
            (import "b" "h" (func $h (result i32)))
            (memory (export "memory") 1)
            (func (export "_start") (result i32) call $h))"#;
        let b = r#"(module
            (import "a" "memory" (memory 1))
            (global $a i32 (i32.const 5))
            (global $b i32 (global.get $a))
            (data (i32.const 8) "\07")
            (func (export "h") (result i32)
              global.get $b
              i32.const 8 i32.load8_u
              i32.add
              (block (param i32) (result i32) i32.const 4 i32.add)
              i32.const 16 i32.const 3 i32.const 2 memory.fill
              i32.const 17 i32.load8_u
              i32.add))"#;
        let sources = vec![
            ImportSources {
                funcs: vec![func(1, 0)],
                ..ImportSources::default()
            },
            ImportSources {
                memory: Some(Source::Memory {
                    module: 0,
                    index: 0,
                }),
                ..ImportSources::default()
            },
        ];
        link_and_run(
            "globals_segments",
            &[wasm(main), wasm(b)],
            sources,
            "let mut inst = Instance::new();\n    assert_eq!(inst.func1(), 19);",
        );
    }

    #[test]
    fn an_appended_module_with_its_own_memory_and_tables_is_renumbered_operator_by_operator() {
        let main = r#"(module
            (import "b" "run" (func $r (result i32)))
            (func (export "_start") (result i32) call $r))"#;
        // Main has no memory or table, so b's become the merged module's (and its second
        // table is merged table 1).
        let b = r#"(module
            (type $r (func (result i32)))
            (memory 1)
            (table $t 4 funcref)
            (table $u 2 funcref (ref.func $one))
            (elem $p func $one $two)
            (data $d "\05")
            (func $one (result i32) i32.const 1)
            (func $two (result i32) i32.const 2)
            (func (export "run") (result i32) (local $acc i32)
              (table.init $t $p (i32.const 0) (i32.const 0) (i32.const 2))
              (elem.drop $p)
              (local.set $acc (call_indirect $t (type $r) (i32.const 1)))
              (local.set $acc (i32.add (local.get $acc) (table.size $u)))
              (local.set $acc (i32.add (local.get $acc) (ref.is_null (table.get $t (i32.const 3)))))
              (local.set $acc (i32.add (local.get $acc)
                (select (result i32) (i32.const 10) (i32.const 20) (i32.const 1))))
              (i32.store8 (i32.const 0) (i32.const 7))
              (local.set $acc (i32.add (local.get $acc) (i32.load8_u (i32.const 0))))
              (table.set $t (i32.const 3) (ref.func $one))
              (local.set $acc (i32.add (local.get $acc) (call_indirect $t (type $r) (i32.const 3))))
              (local.set $acc (i32.add (local.get $acc) (table.grow $u (ref.null func) (i32.const 1))))
              (local.set $acc (i32.add (local.get $acc) (memory.size)))
              (local.set $acc (i32.add (local.get $acc) (memory.grow (i32.const 1))))
              (memory.init $d (i32.const 100) (i32.const 0) (i32.const 1))
              (data.drop $d)
              (local.set $acc (i32.add (local.get $acc) (i32.load8_u (i32.const 100))))
              (table.copy $t $t (i32.const 0) (i32.const 1) (i32.const 1))
              (local.set $acc (i32.add (local.get $acc) (call_indirect $t (type $r) (i32.const 0))))
              (table.fill $u (i32.const 0) (ref.null func) (i32.const 2))
              (local.get $acc)))"#;
        let sources = vec![
            ImportSources {
                funcs: vec![func(1, 2)],
                ..ImportSources::default()
            },
            ImportSources::default(),
        ];
        link_and_run(
            "tables_memories",
            &[wasm(main), wasm(b)],
            sources,
            "let mut inst = Instance::new();\n    assert_eq!(inst.func1(), 34);",
        );
    }

    #[test]
    fn a_second_memory_loops_and_bulk_copies_are_renumbered() {
        let main = r#"(module
            (import "b" "run" (func $r (result i32)))
            (memory (export "memory") 1)
            (func (export "_start") (result i32) call $r))"#;
        let b = r#"(module
            (import "a" "memory" (memory $a 1))
            (memory $m2 1)
            (func (export "run") (result i32) (local $i i32)
              (i64.store $a (i32.const 0) (i64.const 258))
              (memory.copy $m2 $a (i32.const 0) (i32.const 0) (i32.const 8))
              (loop $l
                (local.set $i (i32.add (local.get $i) (i32.const 1)))
                (br_if $l (i32.lt_u (local.get $i) (i32.const 3))))
              (i32.add (i32.wrap_i64 (i64.load $m2 (i32.const 0))) (local.get $i))))"#;
        let sources = vec![
            ImportSources {
                funcs: vec![func(1, 0)],
                ..ImportSources::default()
            },
            ImportSources {
                memory: Some(Source::Memory {
                    module: 0,
                    index: 0,
                }),
                ..ImportSources::default()
            },
        ];
        link_and_run(
            "second_memory",
            &[wasm(main), wasm(b)],
            sources,
            "let mut inst = Instance::new();\n    assert_eq!(inst.func1(), 261);",
        );
    }

    #[test]
    fn each_kind_of_import_reports_a_cycle_a_wrong_provider_or_a_missing_host() {
        let main = r#"(module (global (export "g") i32 (i32.const 1)) (func (export "f")))"#;
        let cases = [
            (r#"(module (import "a" "g" (global i32)))"#, "global"),
            (r#"(module (import "a" "t" (table 1 funcref)))"#, "table"),
            (r#"(module (import "a" "m" (memory 1)))"#, "memory"),
        ];
        let own = |i: usize| match i {
            0 => Source::Global {
                module: 1,
                index: 0,
            },
            1 => Source::Table {
                module: 1,
                index: 0,
            },
            _ => Source::Memory {
                module: 1,
                index: 0,
            },
        };
        let with = |i: usize, source: Source| match i {
            0 => ImportSources {
                globals: vec![source],
                ..ImportSources::default()
            },
            1 => ImportSources {
                tables: vec![source],
                ..ImportSources::default()
            },
            _ => ImportSources {
                memory: Some(source),
                ..ImportSources::default()
            },
        };
        for (i, (module, kind)) in cases.iter().enumerate() {
            let wasms = [wasm(main), wasm(module)];
            let cycle = link_error(&wasms, vec![ImportSources::default(), with(i, own(i))]);
            assert!(cycle.contains("cycle"), "{kind}: {cycle}");
            let wrong = link_error(&wasms, vec![ImportSources::default(), with(i, func(0, 0))]);
            assert!(wrong.contains("provided by"), "{kind}: {wrong}");
            let host = link_error(
                &wasms,
                vec![ImportSources::default(), ImportSources::default()],
            );
            assert!(
                host.contains("host import") && host.contains(kind),
                "{kind}: {host}"
            );
        }
    }

    #[test]
    fn typed_references_and_unfoldable_offsets_in_an_appended_module_are_refused() {
        let main = r#"(module (global (export "g") i32 (i32.const 1)) (func (export "f")))"#;
        let typed = r#"(module (type $t (func)) (func (local (ref null $t))))"#;
        // The provider's global is immutable but reads a host global: not a constant.
        let host_based = r#"(module
            (import "env" "h" (global i32))
            (global (export "g") i32 (global.get 0)))"#;
        let runtime = r#"(module
            (import "a" "g" (global i32))
            (memory 1)
            (data (global.get 0) "x"))"#;
        let none = || vec![ImportSources::default(), ImportSources::default()];
        let what = link_error(&[wasm(main), wasm(typed)], none());
        assert!(what.contains("typed reference"), "{what}");
        let what = link_error(
            &[wasm(host_based), wasm(runtime)],
            vec![
                ImportSources::default(),
                ImportSources {
                    globals: vec![Source::Global {
                        module: 0,
                        index: 1,
                    }],
                    ..ImportSources::default()
                },
            ],
        );
        assert!(what.contains("not an immutable integer constant"), "{what}");
    }

    #[test]
    fn a_data_offset_reading_a_constant_global_of_another_module_is_folded() {
        let main = r#"(module (global (export "base") i32 (i32.const 5)) (memory (export "m") 1)
            (func (export "peek") (param i32) (result i32) local.get 0 i32.load8_u))"#;
        let lib = r#"(module
            (import "a" "base" (global i32))
            (import "a" "m" (memory 1))
            (data (global.get 0) "\2a"))"#;
        link_and_run(
            "folded_data_offset",
            &[wasm(main), wasm(lib)],
            vec![
                ImportSources::default(),
                ImportSources {
                    globals: vec![Source::Global {
                        module: 0,
                        index: 0,
                    }],
                    memory: Some(Source::Memory {
                        module: 0,
                        index: 0,
                    }),
                    ..ImportSources::default()
                },
            ],
            "let mut inst = Instance::new();\n    assert_eq!(inst.func0(5), 42);\n    assert_eq!(inst.func0(4), 0);",
        );
    }

    #[test]
    fn start_functions_run_in_module_order() {
        // main sets g = 10, b adds 1, c doubles: 22 only in that order.
        let main = r#"(module
            (global $g (export "g") (mut i32) (i32.const 0))
            (func $s i32.const 10 global.set $g)
            (func (export "_start") (result i32) global.get $g)
            (start $s))"#;
        let b = r#"(module
            (import "a" "g" (global $g (mut i32)))
            (func $s global.get $g i32.const 1 i32.add global.set $g)
            (start $s))"#;
        let c = r#"(module
            (import "a" "g" (global $g (mut i32)))
            (func $s global.get $g i32.const 2 i32.mul global.set $g)
            (start $s))"#;
        let global = |_: ()| ImportSources {
            globals: vec![Source::Global {
                module: 0,
                index: 0,
            }],
            ..ImportSources::default()
        };
        link_and_run(
            "start_order",
            &[wasm(main), wasm(b), wasm(c)],
            vec![ImportSources::default(), global(()), global(())],
            "let mut inst = Instance::new();\n    assert_eq!(inst.func1(), 22);",
        );
    }

    /// Links `wasms` (main first) with `carried[k]` kept from module `k`.
    fn link_carrying(
        wasms: &[Vec<u8>],
        carried: Vec<Vec<(String, String)>>,
    ) -> Result<String, TranspileError> {
        let topts = TranspileOptions::default();
        let modules = wasms
            .iter()
            .zip(carried)
            .map(|(bytes, carried)| {
                Ok(LinkModule {
                    ir: parse::parse_module(bytes, &topts)?,
                    sources: ImportSources::default(),
                    carried,
                })
            })
            .collect::<Result<Vec<_>, TranspileError>>()?;
        let rewritten = rewrite_bodies(&modules)?;
        let ir = link(modules, &rewritten)?;
        Ok(ir
            .exports
            .iter()
            .map(|e| e.name.clone())
            .collect::<Vec<_>>()
            .join(","))
    }

    fn carry(own: &str, name: &str) -> Vec<(String, String)> {
        vec![(own.to_string(), name.to_string())]
    }

    #[test]
    fn a_carried_export_of_an_appended_module_is_exported_under_the_given_name() {
        let wasms = [
            wasm(r#"(module (func (export "f")))"#),
            wasm(r#"(module (func (export "g")))"#),
        ];
        let exports = link_carrying(&wasms, vec![Vec::new(), carry("g", "renamed")]).unwrap();
        assert_eq!(exports, "f,renamed");
    }

    #[test]
    fn carrying_an_export_the_module_lacks_is_refused() {
        let wasms = [
            wasm(r#"(module (func (export "f")))"#),
            wasm(r#"(module (func (export "g")))"#),
        ];
        let err = link_carrying(&wasms, vec![Vec::new(), carry("missing", "x")]).unwrap_err();
        assert!(err.to_string().contains("`missing`"), "{err}");
    }

    #[test]
    fn carrying_an_export_over_an_existing_name_is_refused() {
        let wasms = [
            wasm(r#"(module (func (export "f")))"#),
            wasm(r#"(module (func (export "g")))"#),
        ];
        let err = link_carrying(&wasms, vec![Vec::new(), carry("g", "f")]).unwrap_err();
        assert!(err.to_string().contains("more than once"), "{err}");
    }

    /// A signature that only appended modules declare gets one canonical id across them, so
    /// a `call_indirect` of one reaches a function of the other.
    #[test]
    fn appended_modules_share_the_canonical_id_of_a_signature_the_main_module_lacks() {
        let main = r#"(module (func (export "f")))"#;
        let b = r#"(module
            (type $t (func (param i64) (result i64)))
            (table (export "t") 1 funcref)
            (elem (i32.const 0) $inc)
            (func $inc (type $t) local.get 0 i64.const 1 i64.add))"#;
        let c = r#"(module
            (import "b" "t" (table 1 funcref))
            (type $t (func (param i64) (result i64)))
            (func (export "g") (result i64) i64.const 41 i32.const 0 call_indirect (type $t)))"#;
        let wasms = [wasm(main), wasm(b), wasm(c)];
        let sources = vec![
            ImportSources::default(),
            ImportSources::default(),
            ImportSources {
                tables: vec![Source::Table {
                    module: 1,
                    index: 0,
                }],
                ..ImportSources::default()
            },
        ];
        // `g` is function 2 of the merged module (main's `f`, then `inc`, then `g`).
        link_and_run(
            "canon_shared",
            &wasms,
            sources,
            "let mut inst = Instance::new();\n    assert_eq!(inst.func2(), 42);",
        );
    }

    #[test]
    fn a_single_appended_start_is_the_merged_start() {
        let main = r#"(module (func (export "f")))"#;
        let b = r#"(module (func $s) (start $s))"#;
        let topts = TranspileOptions::default();
        let (main, b) = (wasm(main), wasm(b));
        let modules = vec![
            LinkModule {
                ir: parse::parse_module(&main, &topts).unwrap(),
                sources: ImportSources::default(),
                carried: Vec::new(),
            },
            LinkModule {
                ir: parse::parse_module(&b, &topts).unwrap(),
                sources: ImportSources::default(),
                carried: Vec::new(),
            },
        ];
        let rewritten = rewrite_bodies(&modules).unwrap();
        let ir = link(modules, &rewritten).unwrap();
        // main defines function 0; b's function 0 is merged function 1.
        assert_eq!(ir.start, Some(1));
        assert_eq!(ir.bodies.len(), 2);
    }

    #[test]
    fn an_operator_outside_the_supported_subset_is_named() {
        let main = r#"(module (func (export "f")))"#;
        let simd = r#"(module (func (result v128) i32.const 1 i32x4.splat))"#;
        let tail = r#"(module (func $f return_call $f))"#;
        for (extra, name) in [(simd, "i32x4_splat"), (tail, "return_call")] {
            let what = link_error(
                &[wasm(main), wasm(extra)],
                vec![ImportSources::default(), ImportSources::default()],
            );
            assert!(what.contains(name), "{what}");
        }
    }

    /// main calls b.g (answered by b); b calls the host import `env.scale` and adds one.
    const MAIN_CALLS_B_ARG: &str = r#"(module
        (import "b" "g" (func $g (param i32) (result i32)))
        (func (export "run") (param i32) (result i32) local.get 0 call $g))"#;
    const B_CALLS_HOST: &str = r#"(module
        (import "env" "scale" (func $scale (param i32) (result i32)))
        (func (export "g") (param i32) (result i32)
          local.get 0 call $scale i32.const 1 i32.add))"#;

    fn host_pair_sources() -> Vec<ImportSources> {
        vec![
            ImportSources {
                funcs: vec![func(1, 1)],
                ..ImportSources::default()
            },
            ImportSources::default(),
        ]
    }

    #[test]
    fn an_appended_host_import_is_called_through_the_host() {
        // main: import 0, run = 1, b.scale (import) = module 1 index 0, b.g = module 1 index 1
        // -> merged 2; b's host import takes the tail slot 3.
        link_and_run(
            "tail_import_run",
            &[wasm(MAIN_CALLS_B_ARG), wasm(B_CALLS_HOST)],
            host_pair_sources(),
            "struct H;\n    impl Imports for H {\n        fn import3(&mut self, a0: i32) -> i32 { a0 * 10 }\n    }\n    let mut inst = Instance::new(H);\n    assert_eq!(inst.func1(5), 51);",
        );
    }

    #[test]
    fn an_appended_host_import_is_a_method_of_the_imports_trait() {
        let generated = linked_source(
            &[wasm(MAIN_CALLS_B_ARG), wasm(B_CALLS_HOST)],
            host_pair_sources(),
        )
        .expect("link and transpile");
        assert!(
            generated.contains("fn import3(&mut self, a0: i32) -> i32;"),
            "{generated}"
        );
    }

    #[test]
    fn named_imports_name_an_appended_host_import_after_the_wasm_import() {
        let topts = TranspileOptions {
            named_imports: true,
            ..TranspileOptions::default()
        };
        let generated = linked_source_with(
            &[wasm(MAIN_CALLS_B_ARG), wasm(B_CALLS_HOST)],
            host_pair_sources(),
            &topts,
        )
        .expect("link and transpile");
        assert!(
            generated.contains("fn env_scale(&mut self, a0: i32) -> i32;"),
            "{generated}"
        );
    }

    #[test]
    fn appended_host_imports_of_several_modules_get_consecutive_slots() {
        // main: imports 0,1, run = 2; b.g = 3; c.h = 4; tail: b.scale = 5, c.bump = 6.
        let main = r#"(module
            (import "b" "g" (func $g (param i32) (result i32)))
            (import "c" "h" (func $h (param i32) (result i32)))
            (func (export "run") (param i32) (result i32)
              local.get 0 call $g call $h))"#;
        let c = r#"(module
            (import "env" "bump" (func $bump (param i32) (result i32)))
            (func (export "h") (param i32) (result i32) local.get 0 call $bump))"#;
        link_and_run(
            "tail_import_two",
            &[wasm(main), wasm(B_CALLS_HOST), wasm(c)],
            vec![
                ImportSources {
                    funcs: vec![func(1, 1), func(2, 1)],
                    ..ImportSources::default()
                },
                ImportSources::default(),
                ImportSources::default(),
            ],
            "struct H;\n    impl Imports for H {\n        fn import5(&mut self, a0: i32) -> i32 { a0 * 10 }\n        fn import6(&mut self, a0: i32) -> i32 { a0 + 100 }\n    }\n    let mut inst = Instance::new(H);\n    assert_eq!(inst.func2(5), 151);",
        );
    }

    #[test]
    fn a_tail_import_can_be_a_call_indirect_target_and_follows_a_synthesized_start() {
        // Both modules start, so a start function is synthesized after the defined
        // functions; the tail slot comes after it.
        let main = r#"(module
            (import "b" "g" (func $g (param i32) (result i32)))
            (global $s (export "s") (mut i32) (i32.const 0))
            (func $init i32.const 7 global.set $s)
            (func (export "run") (param i32) (result i32) local.get 0 call $g)
            (start $init))"#;
        let b = r#"(module
            (import "env" "scale" (func $scale (param i32) (result i32)))
            (table 1 funcref)
            (elem (i32.const 0) $scale)
            (func $nop)
            (func (export "g") (param i32) (result i32)
              local.get 0 i32.const 0 call_indirect (param i32) (result i32))
            (start $nop))"#;
        // main: import 0, init 1, run 2; b: nop 3, g 4; synthesized start 5; tail scale 6.
        link_and_run(
            "tail_import_indirect",
            &[wasm(main), wasm(b)],
            vec![
                ImportSources {
                    funcs: vec![func(1, 2)],
                    ..ImportSources::default()
                },
                ImportSources::default(),
            ],
            "struct H;\n    impl Imports for H {\n        fn import6(&mut self, a0: i32) -> i32 { a0 * 10 }\n    }\n    let mut inst = Instance::new(H);\n    assert_eq!(inst.func2(5), 50);",
        );
    }

    #[test]
    fn a_host_import_chain_through_an_appended_module_reaches_one_tail_slot() {
        // c's import is answered by b's host import, so the call reaches that one slot.
        let main = r#"(module
            (import "c" "h" (func $h (param i32) (result i32)))
            (func (export "run") (param i32) (result i32) local.get 0 call $h))"#;
        let c = r#"(module
            (import "b" "scale" (func $s (param i32) (result i32)))
            (func (export "h") (param i32) (result i32) local.get 0 call $s))"#;
        let b = r#"(module
            (import "env" "scale" (func (param i32) (result i32))))"#;
        // main: import 0, run 1; c.h = 2; b defines nothing; the tail slot is 3.
        link_and_run(
            "tail_import_chain",
            &[wasm(main), wasm(c), wasm(b)],
            vec![
                ImportSources {
                    funcs: vec![func(1, 1)],
                    ..ImportSources::default()
                },
                ImportSources {
                    funcs: vec![func(2, 0)],
                    ..ImportSources::default()
                },
                ImportSources::default(),
            ],
            "struct H;\n    impl Imports for H {\n        fn import3(&mut self, a0: i32) -> i32 { a0 * 10 }\n    }\n    let mut inst = Instance::new(H);\n    assert_eq!(inst.func1(5), 50);",
        );
    }

    #[test]
    fn entry_exports_and_reentrant_hosts_see_an_appended_host_import() {
        let topts = TranspileOptions {
            entry_exports: Some(vec!["run".to_string()]),
            reentrant_imports: true,
            ..TranspileOptions::default()
        };
        let generated = linked_source_with(
            &[wasm(MAIN_CALLS_B_ARG), wasm(B_CALLS_HOST)],
            host_pair_sources(),
            &topts,
        )
        .expect("link and transpile");
        assert!(generated.contains("fn import3(&mut self, g: &mut Instance<Self>"));
        run_source(
            "tail_import_reentrant",
            &generated,
            "struct H;\n    impl Imports for H {\n        fn import3(&mut self, _g: &mut Instance<Self>, a0: i32) -> i32 { a0 * 10 }\n    }\n    let mut inst = Instance::new(H);\n    assert_eq!(inst.func1(5), 51);",
        );
    }

    #[test]
    fn linked_modules_without_an_appended_host_import_have_no_tail_slot() {
        let topts = TranspileOptions::default();
        let wasms = [wasm(MAIN_CALLS_B), wasm(B_USES_A)];
        let modules = wasms
            .iter()
            .zip(two_module_sources())
            .map(|(bytes, sources)| LinkModule {
                ir: parse::parse_module(bytes, &topts).expect("parse"),
                sources,
                carried: Vec::new(),
            })
            .collect::<Vec<_>>();
        let rewritten = rewrite_bodies(&modules).expect("rewrite");
        let ir = link(modules, &rewritten).expect("link");
        assert!(ir.tail_imports.is_empty());
    }

    #[test]
    fn imports_that_cannot_be_bound_are_refused() {
        let main = r#"(module (import "b" "f" (func)) (import "b" "g" (global i32)))"#;
        let b = r#"(module (global (export "g") i32 (i32.const 1)) (func (export "f")))"#;
        let moved_global = ImportSources {
            funcs: vec![Source::Host],
            globals: vec![Source::Global {
                module: 1,
                index: 0,
            }],
            ..ImportSources::default()
        };
        let what = link_error(
            &[wasm(main), wasm(b)],
            vec![moved_global, ImportSources::default()],
        );
        assert!(what.contains("main module imports"), "{what}");

        // b's import is answered by itself.
        let main = r#"(module (func (export "f")))"#;
        let looping = r#"(module (import "b" "f" (func)))"#;
        let what = link_error(
            &[wasm(main), wasm(looping)],
            vec![
                ImportSources::default(),
                ImportSources {
                    funcs: vec![func(1, 0)],
                    ..ImportSources::default()
                },
            ],
        );
        assert!(what.contains("cycle"), "{what}");

        let what = link_error(
            &[wasm(main), wasm(looping)],
            vec![
                ImportSources::default(),
                ImportSources {
                    funcs: vec![Source::Global {
                        module: 0,
                        index: 0,
                    }],
                    ..ImportSources::default()
                },
            ],
        );
        assert!(what.contains("provided by"), "{what}");
    }

    #[test]
    fn an_appended_module_with_gc_types_tags_or_runtime_offsets_is_refused() {
        let main = r#"(module (func (export "f")))"#;
        let gc = r#"(module (type (struct)))"#;
        let tag = r#"(module (tag))"#;
        for (extra, what) in [(gc, "GC"), (tag, "tag")] {
            let got = link_error(
                &[wasm(main), wasm(extra)],
                vec![ImportSources::default(), ImportSources::default()],
            );
            assert!(got.contains(what), "{got}");
        }
    }
}
