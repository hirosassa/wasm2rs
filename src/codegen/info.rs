use wasmparser::{FunctionBody, StorageType, ValType};

use super::{WasiFn, is_exn_hierarchy};

/// Size in bytes of one WebAssembly linear-memory page (64 KiB).
pub(crate) const WASM_PAGE_SIZE: u64 = 65536;
/// The wasm32 hard cap on linear-memory pages (65536 pages × 64 KiB = 4 GiB).
/// `memory.grow` past this returns -1 as the spec requires. Numerically equal
/// to [`WASM_PAGE_SIZE`] by coincidence; the two express distinct quantities.
pub(crate) const WASM_MAX_PAGES: u64 = 65536;

/// Metadata about a module global.
pub(crate) struct GlobalInfo {
    pub ty: ValType,
    pub mutable: bool,
    /// The Rust expression that produces the global's initial value.
    pub init: String,
}

/// Metadata about one of the module's linear memories. The declared maximum
/// bounds `memory.grow` (together with the wasm32 hard cap of 65536 pages) and,
/// for a shared memory, also sizes the reservation.
/// When `imported`, the host owns the buffer (lent through the `Imports` trait)
/// and the instance carries no `memory` field of its own.
pub(crate) struct MemInfo {
    pub min_pages: u64,
    /// The declared maximum, in pages (`memory.grow` fails past it).
    pub max_pages: Option<u64>,
    pub imported: bool,
    /// Whether the memory is declared `shared` (threads proposal). A single
    /// defined shared memory is backed by a thread-shareable, lock-free
    /// `SharedMemory` so its atomics are genuinely atomic across OS threads.
    pub shared: bool,
    /// Whether the memory is declared `i64` (memory64 proposal): its addresses,
    /// `memory.size`/`grow` operands and bulk-operation lengths are `i64`.
    pub memory64: bool,
}

/// Metadata about one of the module's tables (`funcref` or `externref`). `max` is
/// the declared maximum, which `table.grow` enforces.
/// When `imported`, the host owns the `Vec<u32>` storage (lent through the
/// `Imports` trait) and the instance carries no `table` field of its own; the
/// entries are still this module's function indices, so dispatch is unchanged.
pub(crate) struct TableInfo {
    pub min: u32,
    /// The declared maximum number of entries, if any (`table.grow` fails past it).
    pub max: Option<u32>,
    pub imported: bool,
    /// Whether the table is declared `i64` (memory64's table counterpart): its
    /// indices, sizes and lengths are `i64` instead of `i32`.
    pub table64: bool,
    /// The table's element type (`funcref`, `externref` or `exnref`). An `exnref`
    /// table stores managed `GcRef`s, the others `u32` handles (see
    /// [`table_elem_rust`]); `table.get` pushes an operand of this type.
    pub element: ValType,
    /// The value every initial entry holds; `None` starts them null.
    pub init: Option<TableFill>,
}

/// What every initial entry of a table holds when it is not null.
#[derive(Clone, Copy)]
pub(crate) enum TableFill {
    /// The function `(table 2 funcref (ref.func $f))` names.
    Func(u32),
    /// The value of an immutable imported global
    /// (`(table 2 funcref (global.get $imported))`), read from the host when the
    /// instance is built.
    ImportedGlobal(u32),
}

/// Where an active segment lands. Usually a constant, spelled into the generated
/// source; a constant expression reading an imported global is only known to the
/// host, so it is evaluated when the instance is built.
pub(crate) enum SegOffset {
    /// A 64-bit table's or memory's segment can lie above 4 GiB, so it is a `u64`.
    Const(u64),
    /// A Rust expression of type `u64` that reads the host as `imports`.
    Runtime(String),
}

impl SegOffset {
    /// A segment's constant base and, for a runtime offset, its expression. A
    /// passive segment has no offset: `(0, None)`.
    pub(crate) fn split(offset: Option<&Self>) -> (u64, Option<&str>) {
        match offset {
            Some(Self::Const(value)) => (*value, None),
            Some(Self::Runtime(expr)) => (0, Some(expr.as_str())),
            None => (0, None),
        }
    }
}

/// One element segment: function indices for the table. `offset` is `Some` for
/// an active segment (written at its offset during instantiation) and
/// `None` for a passive one (retained for `table.init`). A `declared` segment
/// (neither active nor passive) has no runtime effect but still occupies a slot
/// so that `table.init`/`elem.drop` indices stay aligned with the wasm element
/// index space.
pub(crate) struct ElemSegment {
    /// The table an active segment initialises (0 for a passive or declared one).
    pub table: u32,
    pub offset: Option<SegOffset>,
    pub declared: bool,
    pub funcs: Vec<u32>,
    /// Active items that read an imported global: `(item index, imported global)`.
    /// The host's value is written when the instance is built; `funcs` holds the
    /// null placeholder at that index. A passive segment has none (its items live
    /// in a static, which cannot hold a runtime value).
    pub imported_items: Vec<(usize, u32)>,
}

/// One data segment: raw bytes for linear memory. `offset` is `Some` for an
/// active segment (written at its offset during instantiation) and
/// `None` for a passive one (retained for `memory.init`). `mem_index` is the
/// linear memory the (active) segment initialises; it is 0 for a passive
/// segment (which names no memory until a `memory.init` selects one).
pub(crate) struct DataSegment {
    pub offset: Option<SegOffset>,
    pub mem_index: u32,
    pub bytes: Vec<u8>,
}

/// A function type from the type section: its parameter and result types. Used
/// to resolve a `call_indirect`'s declared type back to a signature.
pub(crate) struct TypeSig {
    pub params: Vec<ValType>,
    pub results: Vec<ValType>,
}

/// One struct field or array element: its storage type, which may be packed
/// (`i8`/`i16`, narrower than any `ValType`). Mutability is not tracked — the
/// wasm binary is assumed already validated, so the transpiler does not re-check
/// writes against a field's `mut` flag.
pub(crate) struct FieldInfo {
    pub storage: StorageType,
}

/// The kind of one entry in the module's type index space. Wasm's GC proposal
/// interleaves function, struct and array types in a single index space, so
/// every index maps to one of these; concrete references (`(ref $t)`) resolve
/// their lowering (`u32` funcref vs managed `GcRef`) through this table. A
/// function type's signature is resolved through the parallel `TypeSig` table
/// (`ModuleCtx::types`), so `Func` carries no payload here.
pub(crate) enum CompositeKind {
    Func,
    Struct(Vec<FieldInfo>),
    Array(FieldInfo),
    /// A continuation type `(cont $ft)` (stack-switching proposal). The payload
    /// is the module type index of the underlying function type `$ft`, whose
    /// signature drives `cont.new`/`resume`/`suspend`. A continuation reference
    /// lowers to a `u32` handle (`u32::MAX` is null), like a `funcref`.
    Cont(u32),
}

/// A function the module exports: its export name and full function index
/// (imports first). Only function exports are recorded.
pub(crate) struct ExportInfo {
    pub name: String,
    pub func_index: u32,
}

/// A global the module exports: its export name and full global index
/// (imported globals first).
pub(crate) struct GlobalExportInfo {
    pub name: String,
    pub global_index: u32,
}

/// An imported function: its signature. Imported functions occupy the low end
/// of the function index space and are dispatched through the injected host,
/// unless `wasi` recognises it as a natively-implemented WASI function.
pub(crate) struct ImportInfo {
    /// The declared function type index.
    pub type_index: u32,
    pub params: Vec<ValType>,
    pub results: Vec<ValType>,
    /// Set when this import is a recognised `wasi_snapshot_preview1` function
    /// that is emitted as an inherent `Instance` method (`self.mem()`-backed)
    /// instead of being dispatched through the host trait.
    pub wasi: Option<WasiFn>,
    /// The module and field name of the import.
    pub module: String,
    pub field: String,
    /// The `Imports` method name under `named_imports`; `None` keeps `import{j}`.
    pub ident: Option<String>,
    /// Set by the core linker when another module of the linked program provides
    /// this import: the full index of the function that answers it. The entry keeps
    /// its slot so the function indices of the module's own code stay valid, but it
    /// is a direct call, not a host method.
    pub alias: Option<u32>,
}

impl ImportInfo {
    /// Whether this import is answered by the host (a method of the `Imports` trait):
    /// not a native WASI function and not an alias of a linked function.
    pub(crate) fn is_host(&self) -> bool {
        self.wasi.is_none() && self.alias.is_none()
    }
}

/// An imported global: its type and mutability. Imported globals occupy the low
/// end of the global index space and are read/written through the injected host
/// (`get_global{k}`/`set_global{k}`), preserving host sharing.
pub(crate) struct ImportedGlobalInfo {
    pub ty: ValType,
    pub mutable: bool,
}

/// A tag: the parameter (and, for control tags, result) types of the underlying
/// function type it names. Tags occupy their own index space (imported tags
/// first, then defined ones), which `throw`/`catch`/`suspend` reference.
/// Exception tags carry parameters but no results; a stack-switching control
/// tag also carries results — the values a `resume` injects when it resumes a
/// continuation suspended on that tag.
pub(crate) struct TagInfo {
    pub params: Vec<ValType>,
    pub results: Vec<ValType>,
}

/// One function to translate: its signature plus its body.
pub(crate) struct FuncInput<'a> {
    /// The declared function type index.
    pub type_index: u32,
    pub params: &'a [ValType],
    pub results: &'a [ValType],
    pub body: &'a FunctionBody<'a>,
}

/// The declared type relation, taken from the validated type section: every type
/// index's canonical id (two types are equivalent exactly when their ids are equal,
/// whatever their rec group) and each canonical id's declared direct supertype.
pub(crate) struct TypeRel {
    pub canon: Vec<u32>,
    pub canon_super: Vec<Option<u32>>,
}

impl TypeRel {
    /// Whether type index `a` is `b` or was declared (transitively) a subtype of it.
    pub(crate) fn is_subtype(&self, a: u32, b: u32) -> bool {
        let (Some(&a), Some(&b)) = (self.canon.get(a as usize), self.canon.get(b as usize)) else {
            return false;
        };
        // The chain is finite for a validated module; bound the walk anyway.
        std::iter::successors(Some(a), |&c| {
            self.canon_super.get(c as usize).copied().flatten()
        })
        .take(self.canon_super.len() + 1)
        .any(|c| c == b)
    }
}

/// Whether a table of `element` references holds managed `GcRef` entries (an
/// `exnref` table) rather than `u32` handles.
pub(crate) fn is_gc_table_element(element: ValType) -> bool {
    matches!(element, ValType::Ref(rt) if is_exn_hierarchy(rt.heap_type()))
}

/// The Rust element type of a table holding `element` references: a managed
/// `GcRef` for an `exnref` table, a `u32` handle (`u32::MAX` is null) otherwise.
pub(crate) fn table_elem_rust(element: ValType) -> &'static str {
    if is_gc_table_element(element) {
        "GcRef"
    } else {
        "u32"
    }
}

/// The Rust expression for a null entry of a table of `element` references.
pub(crate) fn table_null(element: ValType) -> &'static str {
    if is_gc_table_element(element) {
        "GcRef::Null"
    } else {
        "u32::MAX"
    }
}
