//! Per-function code generation.
//!
//! wasm bytecode is already structured — `block`/`loop`/`if` regions nest
//! properly and `br N` can only target one of the `N` enclosing labels, so
//! there is never an irreducible control-flow graph. Each region therefore
//! maps directly onto Rust: a `block` or `loop` that is actually branched to
//! becomes a labelled `loop { ... }` (with `break`/`continue` for `br`), while
//! one that is never targeted is emitted inline to avoid unused-label warnings.
//!
//! Values are tracked on a simulated operand stack of expression strings. A
//! value is "stable" when re-evaluating its expression always yields the same
//! result (a constant, an immutable local, a materialised temporary, or a
//! combination of those). Only non-stable values are spilled into `let`
//! bindings at control-flow boundaries and before local mutations, which keeps
//! straight-line code compiling to clean inline expressions.

use std::collections::HashSet;

use wasmparser::{FunctionBody, MemArg, Operator, ValType};

use crate::TranspileError;

mod const_expr;
mod func;
mod helpers;
mod info;
mod render;
mod runtime;
mod wasi;

use self::func::FuncGen;
use self::helpers::helper_name;
use self::render::{render_chunk_file, render_lib_root, render_module};
use self::runtime::{render_rt_helpers, rt_name};

pub(crate) use self::const_expr::{const_expr_to_rust, const_expr_u32};
pub(crate) use self::info::{
    DataSegment, ElemSegment, FuncInput, GlobalInfo, ImportInfo, ImportedGlobalInfo, MemInfo,
    TableInfo, TypeSig,
};
pub(crate) use self::wasi::WasiFn;

/// A memory-access helper method emitted on the instance `impl` on demand.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Helper {
    LoadI32,
    Load8U,
    Load8S,
    Load16U,
    Load16S,
    LoadI64,
    LoadF32,
    LoadF64,
    Load8UI64,
    Load8SI64,
    Load16UI64,
    Load16SI64,
    Load32UI64,
    Load32SI64,
    StoreI32,
    Store8,
    Store16,
    StoreI64,
    StoreF32,
    StoreF64,
    Store8I64,
    Store16I64,
    Store32I64,
    Grow,
    MemoryFill,
    MemoryCopy,
    TableCopy,
    TableFill,
}

/// A free-standing runtime helper function emitted at module scope on demand.
/// Unlike [`Helper`], these do not touch instance state (memory/globals), so
/// they are plain `fn`s usable from both stateless and stateful modules — used
/// for operations whose wasm semantics differ from Rust's built-in operators.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Rt {
    F32Min,
    F32Max,
    F64Min,
    F64Max,
    I32TruncF32S,
    I32TruncF32U,
    I32TruncF64S,
    I32TruncF64U,
    I64TruncF32S,
    I64TruncF32U,
    I64TruncF64S,
    I64TruncF64U,
}

/// The rendered source of one function plus the helpers it relies on.
struct GenFn {
    src: String,
    /// Instance-method memory helpers.
    helpers: HashSet<Helper>,
    /// Module-scope free-function runtime helpers.
    rt: HashSet<Rt>,
    /// `call_indirect` type indices needing a `call_ref_t{ti}` dispatch method.
    dispatch_sigs: HashSet<u32>,
}

/// The lint-suppression attribute prefixed to generated functions/impls.
const ALLOW: &str =
    "#[allow(dead_code, unused_variables, unused_assignments, unused_mut, unused_parens)]";

/// A value on the simulated operand stack.
#[derive(Clone)]
struct Val {
    /// The Rust expression that produces this value.
    code: String,
    ty: ValType,
    /// Whether re-evaluating `code` is guaranteed to yield the same result.
    stable: bool,
}

/// The kind of a structured control-flow region.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FrameKind {
    Block,
    Loop,
    If,
}

/// One active control-flow region.
struct Frame {
    kind: FrameKind,
    /// Numeric label; only rendered as `'lN` if the frame is branched to.
    label: usize,
    /// Set when some `br`/`br_if`/`br_table` targets this frame.
    targeted: bool,
    /// One `(variable, type)` per result the region yields, in source order.
    results: Vec<(String, ValType)>,
    /// The region's entry parameters (stable operands left on the stack), kept
    /// so an `if`'s `else` arm can restore them after the `then` arm consumes
    /// them.
    entry_params: Vec<Val>,
    /// For a `loop`: one `(variable, type)` per parameter. A `br` back to the
    /// header reassigns these loop-carried variables before `continue`. Empty
    /// for blocks and `if`s.
    loop_params: Vec<(String, ValType)>,
    /// Operand-stack height of the enclosing scope (values below this frame).
    parent_height: usize,
    /// The output buffer of the enclosing scope, restored when the frame ends.
    parent_buffer: Vec<String>,
    /// For `if`: the `then` branch lines, captured when `else` is reached.
    then_buffer: Option<Vec<String>>,
    /// For `if`: whether the `then` branch could fall through to `else`.
    then_reachable: bool,
    /// For `if`: the Rust condition expression.
    cond: Option<String>,
}

impl Frame {
    /// The result variable names, in source order.
    fn result_vars(&self) -> Vec<String> {
        self.results.iter().map(|(var, _)| var.clone()).collect()
    }

    /// The loop-carried parameter variable names, in source order.
    fn loop_param_vars(&self) -> Vec<String> {
        self.loop_params
            .iter()
            .map(|(var, _)| var.clone())
            .collect()
    }
}

/// Module-wide context shared by every function's code generation.
struct ModuleCtx<'a> {
    /// Imported functions, occupying function indices `0..imports.len()`.
    imports: &'a [ImportInfo],
    /// Locally-defined functions, occupying the indices after the imports.
    funcs: &'a [FuncInput<'a>],
    /// Every function type, so `call_indirect` can resolve its declared type
    /// index back to a signature.
    types: &'a [TypeSig],
    /// Per-imported-global `(type, mutable)`, occupying the low global indices.
    imported_globals: Vec<(ValType, bool)>,
    /// Per-defined-global `(type, mutable)`, indexed after the imported globals.
    globals: Vec<(ValType, bool)>,
    /// Whether the module declares linear memory (so `self.mem()` exists).
    has_memory: bool,
    /// Whether the module declares a table (so `self.table()` exists).
    has_table: bool,
    /// Whether the module has an injected host (`self.imports`), so an external
    /// funcref handle in a table can be resolved through the host.
    has_imports: bool,
    /// The table's element type (`funcref` or `externref`), if a table exists;
    /// `table.get` pushes an operand of this type.
    table_element: Option<ValType>,
    /// Per-data-segment: whether it is passive (so `memory.init`/`data.drop`
    /// can reference it through a `data{d}` field), indexed by data index.
    data_passive: Vec<bool>,
    /// Per-element-segment: whether it is passive (so `table.init`/`elem.drop`
    /// can reference it through an `elem{e}` field), indexed by element index.
    elem_passive: Vec<bool>,
    /// Whether functions are emitted as `&mut self` methods (stateful module).
    is_method: bool,
}

impl ModuleCtx<'_> {
    /// The number of functions in the shared index space (imports then defined),
    /// i.e. the range of valid full indices.
    fn func_count(&self) -> usize {
        self.imports.len() + self.funcs.len()
    }

    /// The signature `(params, results)` of the function at full index `fidx`,
    /// spanning imports then defined functions.
    fn full_sig(&self, fidx: usize) -> Option<(&[ValType], &[ValType])> {
        let n_imports = self.imports.len();
        if fidx < n_imports {
            let im = &self.imports[fidx];
            Some((&im.params, &im.results))
        } else {
            let f = self.funcs.get(fidx - n_imports)?;
            Some((f.params, f.results))
        }
    }

    /// The Rust call expression for invoking the function at full index `fidx`
    /// with the given comma-separated argument list. A recognised WASI import is
    /// a native inherent method; any other imported function is dispatched
    /// through the injected host; defined ones are (method) calls.
    fn call_expr(&self, fidx: usize, arg_list: &str) -> String {
        if let Some(im) = self.imports.get(fidx) {
            // A recognised WASI import is a native inherent method; any other
            // import is dispatched through the injected host trait.
            return match im.wasi {
                Some(w) => format!("self.{}({arg_list})", w.method()),
                None => format!("self.imports.import{fidx}({arg_list})"),
            };
        }
        if self.is_method {
            format!("self.func{fidx}({arg_list})")
        } else {
            format!("func{fidx}({arg_list})")
        }
    }
}

/// The borrowed, raw inputs describing one module, as gathered by the parser.
///
/// Bundling them keeps the module-level entry points (`generate_module` and
/// `render_module`) to a small, stable argument list; the derived translation
/// context ([`ModuleCtx`]) is computed from these.
pub(crate) struct ModuleParts<'a> {
    pub(crate) imports: &'a [ImportInfo],
    pub(crate) imported_globals: &'a [ImportedGlobalInfo],
    pub(crate) funcs: &'a [FuncInput<'a>],
    pub(crate) types: &'a [TypeSig],
    pub(crate) globals: &'a [GlobalInfo],
    pub(crate) memory: Option<&'a MemInfo>,
    pub(crate) data: &'a [DataSegment],
    pub(crate) table: Option<&'a TableInfo>,
    pub(crate) elements: &'a [ElemSegment],
}

/// Derive the translation context from a module's raw parts.
///
/// Returns the [`ModuleCtx`] plus whether the module is *stateful* — i.e.
/// carries mutable state (memory, a table, globals or imports) and is therefore
/// emitted as a `struct Instance` with `&mut self` methods rather than free
/// functions. Also performs the module-level validation that cannot be checked
/// during parsing (a native WASI function that reads memory needs one).
fn build_ctx<'a>(parts: &ModuleParts<'a>) -> Result<(ModuleCtx<'a>, bool), TranspileError> {
    let ModuleParts {
        imports,
        imported_globals,
        funcs,
        types,
        globals,
        memory,
        data,
        table,
        elements,
        ..
    } = *parts;

    let has_memory = memory.is_some();
    let has_table = table.is_some();
    // A native WASI function that reads/writes linear memory (e.g. `fd_write`)
    // is emitted with `self.mem()`, which only exists when the module has a
    // memory; reject a module that imports one but declares none.
    if !has_memory
        && imports
            .iter()
            .any(|im| im.wasi.is_some_and(WasiFn::needs_memory))
    {
        return Err(TranspileError::Unsupported(
            "native WASI memory access without a linear memory".into(),
        ));
    }
    // The host is injected whenever anything is imported (globals, non-WASI
    // functions, or host-owned memory/table).
    let has_imports = !imports.is_empty()
        || !imported_globals.is_empty()
        || memory.is_some_and(|m| m.imported)
        || table.is_some_and(|t| t.imported);
    // Imports must be held by an instance, so a module that has them (or any
    // other mutable state) becomes a `struct Instance` with method functions.
    let stateful = has_memory || has_table || has_imports || !globals.is_empty();

    let ctx = ModuleCtx {
        imports,
        funcs,
        types,
        imported_globals: imported_globals.iter().map(|g| (g.ty, g.mutable)).collect(),
        globals: globals.iter().map(|g| (g.ty, g.mutable)).collect(),
        has_memory,
        has_table,
        has_imports,
        table_element: table.map(|t| t.element),
        data_passive: data.iter().map(|d| d.offset.is_none()).collect(),
        elem_passive: elements
            .iter()
            .map(|e| e.offset.is_none() && !e.declared)
            .collect(),
        is_method: stateful,
    };
    Ok((ctx, stateful))
}

/// Translate a whole module into a single Rust source string.
///
/// A module that declares linear memory, a table or globals carries mutable
/// state, so it is emitted as a `pub struct Instance` with the functions as
/// `&mut self` methods. A stateless module keeps its functions as free
/// `pub fn`s, matching the earlier phases exactly.
pub(crate) fn generate_module(parts: &ModuleParts<'_>) -> Result<String, TranspileError> {
    let (ctx, stateful) = build_ctx(parts)?;

    let mut sources = Vec::with_capacity(parts.funcs.len());
    let mut used: HashSet<Helper> = HashSet::new();
    let mut used_rt: HashSet<Rt> = HashSet::new();
    let mut dispatch_sigs: HashSet<u32> = HashSet::new();
    for (index, f) in parts.funcs.iter().enumerate() {
        // Defined functions are named by their full function index, i.e. after
        // the imported functions in the shared index space.
        let generated = generate_function(parts.imports.len() + index, f, &ctx)?;
        used.extend(generated.helpers);
        used_rt.extend(generated.rt);
        dispatch_sigs.extend(generated.dispatch_sigs);
        sources.push(generated.src);
    }

    // Free-function runtime helpers live at module scope, above the functions
    // (or the `struct Instance`) that call them, in both module shapes.
    let rt_helpers = render_rt_helpers(&used_rt);

    let body = if !stateful {
        sources.join("\n")
    } else {
        render_module(parts, &ctx, &sources, &used, &dispatch_sigs)?
    };

    Ok(if rt_helpers.is_empty() {
        body
    } else {
        format!("{rt_helpers}\n{body}")
    })
}

/// Translate a module, emitting its Rust source across one or more files.
///
/// Each finished file is handed to `emit(name, code)` and then dropped, so the
/// peak memory stays around one chunk's worth rather than the whole program.
/// When nothing forces a split — the module fits in `funcs_per_file` functions
/// and no `max_bytes_per_file` cap is set — the output is byte-identical to
/// [`generate_module`], emitted as a single `lib.rs`.
///
/// Otherwise the defined functions are chunked into `funcs_{n}.rs` files and a
/// `lib.rs` root ties them together: for a stateless module the chunks hold free
/// `pub fn`s re-exported from the root; for a stateful one each chunk adds an
/// `impl Instance` block while the root owns the struct, `new`, the shared
/// helper methods and the module-scope runtime helpers.
///
/// A chunk is flushed once it reaches `funcs_per_file` functions *or* its
/// accumulated source reaches `max_bytes_per_file` bytes (whichever comes
/// first). The byte cap is what actually bounds peak memory when a module holds
/// a few very large functions, since a fixed function count can still add up to
/// a huge chunk. Both caps take effect only at a function boundary, so a single
/// oversized function is still emitted whole.
pub(crate) fn generate_module_split(
    parts: &ModuleParts<'_>,
    funcs_per_file: usize,
    max_bytes_per_file: usize,
    emit: &mut dyn FnMut(String, String) -> Result<(), TranspileError>,
) -> Result<(), TranspileError> {
    let per = if funcs_per_file == 0 {
        usize::MAX
    } else {
        funcs_per_file
    };
    let byte_cap = if max_bytes_per_file == 0 {
        usize::MAX
    } else {
        max_bytes_per_file
    };
    // With nothing forcing a split, keep the exact single-file rendering.
    if parts.funcs.len() <= per && byte_cap == usize::MAX {
        let code = generate_module(parts)?;
        return emit("lib.rs".to_string(), code);
    }

    let (ctx, stateful) = build_ctx(parts)?;
    let base = parts.imports.len();

    // Aggregated across every function: needed only to render the `lib.rs` root
    // (helper methods, dispatch methods and runtime helpers). Each chunk file is
    // otherwise self-contained, so it can be emitted and dropped immediately.
    let mut used: HashSet<Helper> = HashSet::new();
    let mut used_rt: HashSet<Rt> = HashSet::new();
    let mut dispatch_sigs: HashSet<u32> = HashSet::new();

    let mut chunk: Vec<String> = Vec::new();
    let mut chunk_bytes = 0usize;
    let mut chunk_index = 0usize;
    for (index, f) in parts.funcs.iter().enumerate() {
        let generated = generate_function(base + index, f, &ctx)?;
        used.extend(generated.helpers);
        used_rt.extend(generated.rt);
        dispatch_sigs.extend(generated.dispatch_sigs);
        chunk_bytes += generated.src.len();
        chunk.push(generated.src);
        if chunk.len() >= per || chunk_bytes >= byte_cap {
            let code = render_chunk_file(parts, stateful, &chunk);
            emit(format!("funcs_{chunk_index}.rs"), code)?;
            chunk.clear();
            chunk_bytes = 0;
            chunk_index += 1;
        }
    }
    if !chunk.is_empty() {
        let code = render_chunk_file(parts, stateful, &chunk);
        emit(format!("funcs_{chunk_index}.rs"), code)?;
        chunk_index += 1;
    }
    let n_chunks = chunk_index;

    // The root is emitted last, once the used-helper/dispatch sets are complete.
    let root = render_lib_root(
        parts,
        &ctx,
        stateful,
        &used,
        &used_rt,
        &dispatch_sigs,
        n_chunks,
    )?;
    emit("lib.rs".to_string(), root)
}

fn generate_function(
    index: usize,
    input: &FuncInput<'_>,
    ctx: &ModuleCtx<'_>,
) -> Result<GenFn, TranspileError> {
    let mut func = FuncGen::new(input.params, input.results, input.body, ctx)?;
    func.run(input.body)?;
    func.finish(index, input.params, input.results)
}

/// The offset field of a memory access, as a `u32` (32-bit memory only).
fn memarg_offset(memarg: MemArg) -> Result<u32, TranspileError> {
    u32::try_from(memarg.offset)
        .map_err(|_| TranspileError::Unsupported("memory offset too large".into()))
}

/// Render bytes as a comma-separated list of `u8` literals (a Rust array body).
///
/// The list is broken onto a new line every `PER_LINE` bytes: a large data
/// segment would otherwise render as one multi-megabyte line that overflows
/// rustc's parser. The embedded newlines sit inside the `[ ... ]` wrapper at the
/// call site, producing an ordinary multi-line array literal.
fn byte_array_literal(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    const PER_LINE: usize = 32;
    let mut out = String::new();
    for (i, b) in bytes.iter().enumerate() {
        out.push_str(if i % PER_LINE == 0 { "\n" } else { " " });
        let _ = write!(out, "{b}u8,");
    }
    out
}

/// Indent each non-empty line by four spaces.
fn indent(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .map(|l| {
            if l.is_empty() {
                String::new()
            } else {
                format!("    {l}")
            }
        })
        .collect()
}

/// Collect the indices of locals written by `local.set`/`local.tee`.
fn collect_mutated_locals(body: &FunctionBody<'_>) -> Result<HashSet<u32>, TranspileError> {
    let mut mutated = HashSet::new();
    for op in body.get_operators_reader()? {
        match op? {
            Operator::LocalSet { local_index } | Operator::LocalTee { local_index } => {
                mutated.insert(local_index);
            }
            _ => {}
        }
    }
    Ok(mutated)
}

/// Whether control can reach the code following a finished frame.
fn reachable_after(frame: &Frame, reachable_at_end: bool) -> bool {
    match frame.kind {
        // A loop is only exited by falling through its end.
        FrameKind::Loop => reachable_at_end,
        // A block/if is exited by fall-through or by a `br` that targets it. An
        // `if` without an `else` always has the condition-false fall-through.
        FrameKind::Block => reachable_at_end || frame.targeted,
        FrameKind::If => {
            if frame.then_buffer.is_none() {
                return true;
            }
            reachable_at_end || frame.then_reachable || frame.targeted
        }
    }
}

fn rust_type(ty: ValType) -> Result<&'static str, TranspileError> {
    match ty {
        ValType::I32 => Ok("i32"),
        ValType::I64 => Ok("i64"),
        ValType::F32 => Ok("f32"),
        ValType::F64 => Ok("f64"),
        // A `funcref` is a function index and an `externref` is an opaque host
        // handle; both are represented as a `u32` (`u32::MAX` is null), matching
        // the table's element representation.
        ValType::Ref(rt) if rt.is_func_ref() || rt.is_extern_ref() => Ok("u32"),
        other => Err(TranspileError::Unsupported(format!("value type {other:?}"))),
    }
}

/// The Rust type name of each value type, in order.
fn rust_types(tys: &[ValType]) -> Result<Vec<&'static str>, TranspileError> {
    tys.iter().map(|ty| rust_type(*ty)).collect()
}

/// The unsigned integer type used to reinterpret `ty` for unsigned operations.
fn unsigned_type(ty: ValType) -> Result<&'static str, TranspileError> {
    match ty {
        ValType::I32 => Ok("u32"),
        ValType::I64 => Ok("u64"),
        other => Err(TranspileError::Unsupported(format!(
            "unsigned operation on {other:?}"
        ))),
    }
}

fn default_value(ty: ValType) -> &'static str {
    match ty {
        ValType::F32 | ValType::F64 => "0.0",
        // A default `funcref`/`externref` is null.
        ValType::Ref(rt) if rt.is_func_ref() || rt.is_extern_ref() => "u32::MAX",
        _ => "0",
    }
}

/// Render an `i32` constant as a valid Rust expression. `i32::MIN` cannot be
/// written as the literal `-2147483648i32` (Rust parses that as negation of the
/// out-of-range literal `2147483648i32`), so it uses the associated constant.
fn i32_literal(value: i32) -> String {
    if value == i32::MIN {
        "i32::MIN".to_string()
    } else {
        format!("{value}i32")
    }
}

fn index_u32(i: usize) -> Result<u32, TranspileError> {
    u32::try_from(i).map_err(|_| TranspileError::Unsupported("index too large".into()))
}

fn i64_literal(value: i64) -> String {
    if value == i64::MIN {
        "i64::MIN".to_string()
    } else {
        format!("{value}i64")
    }
}
