use wasmparser::{FunctionBody, ValType};

use super::WasiFn;

/// Metadata about a module global.
pub(crate) struct GlobalInfo {
    pub ty: ValType,
    pub mutable: bool,
    /// The Rust expression that produces the global's initial value.
    pub init: String,
}

/// Metadata about the module's linear memory. The declared maximum is not
/// tracked; `memory.grow` only enforces the wasm32 hard cap of 65536 pages.
/// When `imported`, the host owns the buffer (lent through the `Imports` trait)
/// and the instance carries no `memory` field of its own.
pub(crate) struct MemInfo {
    pub min_pages: u64,
    pub imported: bool,
}

/// Metadata about the module's function table (a single `funcref` table). The
/// declared maximum is not tracked; growth beyond `min` is not supported yet.
/// When `imported`, the host owns the `Vec<u32>` storage (lent through the
/// `Imports` trait) and the instance carries no `table` field of its own; the
/// entries are still this module's function indices, so dispatch is unchanged.
pub(crate) struct TableInfo {
    pub min: u32,
    pub imported: bool,
    /// The table's element type (`funcref` or `externref`). Both are stored as a
    /// `u32`, but `table.get` pushes an operand of this type.
    pub element: ValType,
}

/// One element segment: function indices for the table. `offset` is `Some` for
/// an active segment (written at that constant offset during instantiation) and
/// `None` for a passive one (retained for `table.init`). A `declared` segment
/// (neither active nor passive) has no runtime effect but still occupies a slot
/// so that `table.init`/`elem.drop` indices stay aligned with the wasm element
/// index space.
pub(crate) struct ElemSegment {
    pub offset: Option<u32>,
    pub declared: bool,
    pub funcs: Vec<u32>,
}

/// One data segment: raw bytes for linear memory. `offset` is `Some` for an
/// active segment (written at that constant offset during instantiation) and
/// `None` for a passive one (retained for `memory.init`).
pub(crate) struct DataSegment {
    pub offset: Option<u32>,
    pub bytes: Vec<u8>,
}

/// A function type from the type section: its parameter and result types. Used
/// to resolve a `call_indirect`'s declared type back to a signature.
pub(crate) struct TypeSig {
    pub params: Vec<ValType>,
    pub results: Vec<ValType>,
}

/// An imported function: its signature. Imported functions occupy the low end
/// of the function index space and are dispatched through the injected host,
/// unless `wasi` recognises it as a natively-implemented WASI function.
pub(crate) struct ImportInfo {
    pub params: Vec<ValType>,
    pub results: Vec<ValType>,
    /// Set when this import is a recognised `wasi_snapshot_preview1` function
    /// that is emitted as an inherent `Instance` method (`self.mem()`-backed)
    /// instead of being dispatched through the host trait.
    pub wasi: Option<WasiFn>,
}

/// An imported global: its type and mutability. Imported globals occupy the low
/// end of the global index space and are read/written through the injected host
/// (`get_global{k}`/`set_global{k}`), preserving host sharing.
pub(crate) struct ImportedGlobalInfo {
    pub ty: ValType,
    pub mutable: bool,
}

/// One function to translate: its signature plus its body.
pub(crate) struct FuncInput<'a> {
    pub params: &'a [ValType],
    pub results: &'a [ValType],
    pub body: &'a FunctionBody<'a>,
}
