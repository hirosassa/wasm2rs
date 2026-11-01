use super::*;

use std::collections::{HashMap, HashSet};

use wasmparser::ValType;

use crate::TranspileError;

/// The typed-continuations analysis for one module, grouped so the four tightly
/// coupled tables travel together and the call sites read `ctx.cont.bodies` etc.
/// rather than four flat `ModuleCtx` fields.
pub(crate) struct ContInfo {
    /// Function indices reachable as continuation bodies (the target of a
    /// `ref.func` fed into `cont.new`), sorted and deduplicated. These are
    /// emitted as resumable `cont_step_func{N}` state machines rather than
    /// ordinary `func{N}`s, and drive the `ContObj`/`ContFrame` runtime types.
    /// These are the resumable *entry points* (handles created by `cont.new`).
    pub(crate) bodies: Vec<u32>,
    /// Every function emitted as a `cont_step_func{N}`: the continuation bodies
    /// plus every function reachable from one through a suspend-crossing `call`.
    /// A superset of [`bodies`](Self::bodies), sorted. Each gets a `ContFrame{N}`,
    /// but only `bodies` get a `ContObj` variant and a `conts`-table handle.
    pub(crate) step_set: Vec<u32>,
    /// Per step function, the callee of its tail cross-call checkpoint (a `call`
    /// to another step function). The callee's frame nests inside the caller's
    /// `ContFrame` as its `sub` field.
    pub(crate) checkpoint_callee: HashMap<u32, u32>,
    /// Per step function, its local types by index (`l{i}`). Each becomes a
    /// `ContFrame` field so the local survives suspends. Step functions have no
    /// parameters (rejected), so these are exactly the declared locals.
    pub(crate) step_locals: HashMap<u32, Vec<ValType>>,
}

/// The per-tag payload/result types for one module, grouped so `ctx.tags.params`
/// and `ctx.tags.results` stay together. Both are indexed by tag index (imported
/// tags first, then defined); named `TagTypes` to avoid colliding with the
/// parser's [`TagInfo`].
pub(crate) struct TagTypes {
    /// Per-tag payload (parameter) types. `throw`/`catch`/`suspend` resolve their
    /// tag index here.
    pub(crate) params: Vec<Vec<ValType>>,
    /// Per-tag result types. Empty for an exception tag; for a stack-switching
    /// control tag these are the values a `resume` injects back into the
    /// continuation it resumes.
    pub(crate) results: Vec<Vec<ValType>>,
}

/// Module-wide context shared by every function's code generation.
pub(crate) struct ModuleCtx<'a> {
    /// Imported functions, occupying function indices `0..imports.len()`.
    pub(crate) imports: &'a [ImportInfo],
    /// Locally-defined functions, occupying the indices after the imports.
    pub(crate) funcs: &'a [FuncInput<'a>],
    /// Function indices (imports then defined, ascending) grouped by exact
    /// `(params, results)` signature: the set of functions a `call_indirect`
    /// of that signature can reach. Built once so each call site and dispatch
    /// method looks it up instead of rescanning the whole function space.
    funcs_by_sig: SigIndex,
    /// Every function type, so `call_indirect` can resolve its declared type
    /// index back to a signature.
    pub(crate) types: &'a [TypeSig],
    /// The kind (function / struct / array) of every type index, so a concrete
    /// reference and the struct/array operators can resolve field/element
    /// storage and pick the right value lowering.
    pub(crate) type_kinds: &'a [CompositeKind],
    /// Per type index, every concrete struct/array type whose super chain
    /// reaches it (see [`ModuleCtx::concrete_descendants`]), ascending. Built
    /// once so each `ref.test`/`ref.cast` site looks it up instead of walking
    /// the whole hierarchy.
    descendants: Vec<Vec<u32>>,
    /// Per-imported-global `(type, mutable)`, occupying the low global indices.
    pub(crate) imported_globals: Vec<(ValType, bool)>,
    /// Per-defined-global `(type, mutable)`, indexed after the imported globals.
    pub(crate) globals: Vec<(ValType, bool)>,
    /// Whether the module declares linear memory (so `self.mem()` exists).
    pub(crate) has_memory: bool,
    /// The number of linear memories (imported first, then defined). Memory
    /// operations carry a static index that must be below this.
    pub(crate) n_memories: usize,
    /// Per memory (indexed like the memory operands), whether it is 64-bit
    /// (memory64 proposal), so its addresses and sizes are `i64`.
    pub(crate) memory64: Vec<bool>,
    /// True iff the module has exactly one memory and it is `shared`. When set,
    /// linear memory is backed by a thread-shareable `SharedMemory` (Mutex) and
    /// the atomic RMW/cmpxchg/wait/notify ops lower to genuine atomics.
    pub(crate) memory_shared: bool,
    /// Whether the module declares a table (so `self.table()` exists).
    pub(crate) has_table: bool,
    /// Whether the module has an injected host (`self.imports`), so an external
    /// funcref handle in a table can be resolved through the host.
    pub(crate) has_imports: bool,
    /// The table's element type (`funcref` or `externref`), if a table exists;
    /// `table.get` pushes an operand of this type.
    pub(crate) table_element: Option<ValType>,
    /// Per-data-segment: whether it is passive (so `memory.init`/`data.drop`
    /// can reference it through a `data{d}` field), indexed by data index.
    pub(crate) data_passive: Vec<bool>,
    /// Per-element-segment: whether it is passive (so `table.init`/`elem.drop`
    /// can reference it through an `elem{e}` field), indexed by element index.
    pub(crate) elem_passive: Vec<bool>,
    /// Whether functions are emitted as `&mut self` methods (stateful module).
    pub(crate) is_method: bool,
    /// Whether any body uses `extern.convert_any`/`any.convert_extern`, which
    /// internalise `anyref`s through the per-instance `extern_box: Vec<GcRef>`.
    /// Emits (and initialises) that field.
    pub(crate) uses_extern_box: bool,
    /// Per-tag payload/result types (`ctx.tags.params` / `ctx.tags.results`).
    pub(crate) tags: TagTypes,
    /// The typed-continuations analysis (`ctx.cont.bodies`/`step_set`/…).
    pub(crate) cont: ContInfo,
    /// Opt-in: emit unchecked (`unsafe`) linear-memory access, dropping the
    /// slice bounds check on every load/store and bulk `memory.*`. Off by
    /// default; when set, an out-of-bounds access is undefined behaviour rather
    /// than a wasm trap, so it is only sound for trusted modules.
    pub(crate) unsafe_memory: bool,
    /// Cap on match arms per flattened-dispatch *part* function. When a flattened
    /// `loop { match pc { … } }` has more surviving arms than this, it is split
    /// into `ceil(arms / split_dispatch)` sibling part functions over a shared
    /// state struct. `0` (the default) keeps each flattened function whole.
    pub(crate) split_dispatch: usize,
}
impl ModuleCtx<'_> {
    /// Every concrete struct/array type index whose declared supertype chain
    /// reaches `target` (including `target` itself when it is a struct/array),
    /// ascending; empty for an unknown `target`.
    ///
    /// The whole type hierarchy is known statically, so a runtime downcast to a
    /// static `target` reduces to membership in this set of concrete type ids.
    pub(crate) fn concrete_descendants(&self, target: u32) -> &[u32] {
        self.descendants
            .get(target as usize)
            .map_or(&[], Vec::as_slice)
    }

    /// The signature `(params, results)` of the function at full index `fidx`,
    /// spanning imports then defined functions.
    pub(crate) fn full_sig(&self, fidx: usize) -> Option<(&[ValType], &[ValType])> {
        let n_imports = self.imports.len();
        if fidx < n_imports {
            let im = &self.imports[fidx];
            Some((&im.params, &im.results))
        } else {
            let f = self.funcs.get(fidx - n_imports)?;
            Some((f.params, f.results))
        }
    }

    /// The full indices of every function whose signature is exactly
    /// `(params, results)`, ascending. Empty when none matches.
    pub(crate) fn funcs_with_sig(&self, params: &[ValType], results: &[ValType]) -> &[usize] {
        self.funcs_by_sig
            .get(&(params.to_vec(), results.to_vec()))
            .map_or(&[], Vec::as_slice)
    }

    /// The Rust call expression for invoking the function at full index `fidx`
    /// with the given comma-separated argument list. A recognised WASI import is
    /// a native inherent method; any other imported function is dispatched
    /// through the injected host; defined ones are (method) calls.
    pub(crate) fn call_expr(&self, fidx: usize, arg_list: &str) -> String {
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
    /// Parallel to `funcs`: whether each defined function is emitted. A dead
    /// function (see `TranspileOptions::entry_exports`) is skipped and left out
    /// of every `call_indirect` dispatch, but keeps its index and name.
    pub(crate) live: &'a [bool],
    pub(crate) types: &'a [TypeSig],
    pub(crate) type_kinds: &'a [CompositeKind],
    /// The declared direct supertype of every type index (`None` when none),
    /// parallel to `type_kinds`. Drives the runtime subtype checks.
    pub(crate) supers: &'a [Option<u32>],
    pub(crate) globals: &'a [GlobalInfo],
    /// Every linear memory, in index order (imported memories first, then
    /// defined). Empty when the module declares none.
    pub(crate) memories: &'a [MemInfo],
    pub(crate) data: &'a [DataSegment],
    pub(crate) table: Option<&'a TableInfo>,
    pub(crate) elements: &'a [ElemSegment],
    pub(crate) tags: &'a [TagInfo],
    /// The module's function exports, in declaration order.
    pub(crate) exports: &'a [ExportInfo],
    /// The module's `start` function index, run at the end of every instantiation.
    pub(crate) start: Option<u32>,
    /// Opt-in unchecked linear-memory access (see [`ModuleCtx::unsafe_memory`]).
    pub(crate) unsafe_memory: bool,
    /// Cap on match arms per flattened-dispatch part function; `0` disables the
    /// split (see [`ModuleCtx::split_dispatch`]).
    pub(crate) split_dispatch: usize,
}
/// Derive the translation context from a module's raw parts.
///
/// Returns the [`ModuleCtx`] plus whether the module is *stateful* — i.e.
/// carries mutable state (memory, a table, globals or imports) and is therefore
/// emitted as a `struct Instance` with `&mut self` methods rather than free
/// functions. Also performs the module-level validation that cannot be checked
/// during parsing (a native WASI function that reads memory needs one).
pub(crate) fn build_ctx<'a>(
    parts: &ModuleParts<'a>,
) -> Result<(ModuleCtx<'a>, bool), TranspileError> {
    let ModuleParts {
        imports,
        imported_globals,
        funcs,
        types,
        type_kinds,
        supers,
        globals,
        memories,
        data,
        table,
        elements,
        ..
    } = *parts;

    let has_memory = !memories.is_empty();
    let has_table = table.is_some();
    // The thread-shareable backend supports only a single defined shared memory
    // (index 0). More than one memory where any is shared is out of scope.
    if memories.len() > 1 && memories.iter().any(|m| m.shared) {
        return Err(TranspileError::Unsupported(
            "shared memory with multiple memories".into(),
        ));
    }
    let memory_shared = memories.len() == 1 && memories.iter().any(|m| m.shared);
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
    // `wasi::thread-spawn` starts each thread as a sibling instance over the
    // same shared memory and enters it through the exported `wasi_thread_start`.
    if imports
        .iter()
        .any(|im| im.wasi == Some(WasiFn::ThreadSpawn))
    {
        if !memory_shared {
            return Err(TranspileError::Unsupported(
                "wasi::thread-spawn requires a shared memory".into(),
            ));
        }
        if !parts.exports.iter().any(|e| e.name == "wasi_thread_start") {
            return Err(TranspileError::Unsupported(
                "wasi::thread-spawn requires an exported wasi_thread_start".into(),
            ));
        }
    }
    // `env.__pthread_create_js` replays an emscripten Worker's start-up on a
    // sibling instance, which needs the shared memory and a table owning the
    // start routine.
    if imports
        .iter()
        .any(|im| im.wasi == Some(WasiFn::PthreadCreateJs))
    {
        if !memory_shared {
            return Err(TranspileError::Unsupported(
                "env.__pthread_create_js requires a shared memory".into(),
            ));
        }
        if table.is_none_or(|t| t.imported) {
            return Err(TranspileError::Unsupported(
                "env.__pthread_create_js requires a module-defined table".into(),
            ));
        }
    }
    // The host is injected whenever anything is imported (globals, non-WASI
    // functions, or host-owned memory/table).
    let has_imports = !imports.is_empty()
        || !imported_globals.is_empty()
        || memories.iter().any(|m| m.imported && !m.shared)
        || table.is_some_and(|t| t.imported);
    // Imports must be held by an instance, so a module that has them (or any
    // other mutable state) becomes a `struct Instance` with method functions.
    // `call_ref`/`return_call_ref` also require the enclosing instance: they
    // dispatch through the `self.call_ref_t{ti}` method, which only exists on a
    // `struct Instance`, so a body that uses either forces statefulness even
    // when the module carries no other state. (`ref.func` alone does not — it
    // just pushes a `u32`.)
    // A single pass over every body collects the statefulness-forcing operator
    // flags and the continuation suspend/call tables together, instead of five
    // independent full scans.
    let scan = scan_bodies(funcs)?;
    let uses_extern_box = scan.uses_extern_box;
    // Continuations carry per-instance state (the `conts` handle table), so any
    // module that creates one is stateful even without other mutable state.
    let cont_bodies = scan.cont_bodies;
    let n_imports = imports.len();
    let can_suspend = can_suspend_functions(n_imports, &scan.suspends, &scan.calls)?;
    let (step_set, checkpoint_callee) =
        step_functions(n_imports, &cont_bodies, &can_suspend, &scan.calls)?;
    // A function that can suspend but is not a step function would be emitted as
    // an ordinary `func{N}` and choke on its `suspend`; it is either dead or a
    // misuse (suspending outside any continuation). Surface it cleanly.
    for f in &can_suspend {
        if step_set.binary_search(f).is_err() {
            return Err(TranspileError::Unsupported(
                "function can suspend but is not reachable as a continuation".into(),
            ));
        }
    }
    reject_dual_use_continuations(funcs, elements, &step_set, n_imports)?;
    let step_locals = step_function_locals(funcs, n_imports, &step_set)?;
    let stateful = has_memory
        || has_table
        || has_imports
        || !globals.is_empty()
        || scan.uses_call_ref
        || scan.uses_array_segment_ops
        || uses_extern_box
        || !cont_bodies.is_empty()
        || parts.start.is_some();

    let funcs_by_sig = index_by_signature(
        imports
            .iter()
            .map(|im| (im.params.as_slice(), im.results.as_slice()))
            .map(Some)
            .chain(
                funcs
                    .iter()
                    .zip(parts.live)
                    .map(|(f, live)| live.then_some((f.params, f.results))),
            ),
    );
    let ctx = ModuleCtx {
        imports,
        funcs,
        funcs_by_sig,
        types,
        type_kinds,
        descendants: index_descendants(type_kinds, supers),
        imported_globals: imported_globals.iter().map(|g| (g.ty, g.mutable)).collect(),
        globals: globals.iter().map(|g| (g.ty, g.mutable)).collect(),
        has_memory,
        n_memories: memories.len(),
        memory64: memories.iter().map(|m| m.memory64).collect(),
        memory_shared,
        has_table,
        has_imports,
        table_element: table.map(|t| t.element),
        data_passive: data.iter().map(|d| d.offset.is_none()).collect(),
        elem_passive: elements
            .iter()
            .map(|e| e.offset.is_none() && !e.declared)
            .collect(),
        is_method: stateful,
        uses_extern_box,
        tags: TagTypes {
            params: parts.tags.iter().map(|t| t.params.clone()).collect(),
            results: parts.tags.iter().map(|t| t.results.clone()).collect(),
        },
        cont: ContInfo {
            bodies: cont_bodies,
            step_set,
            checkpoint_callee,
            step_locals,
        },
        unsafe_memory: parts.unsafe_memory,
        split_dispatch: parts.split_dispatch,
    };
    Ok((ctx, stateful))
}
/// The module-wide helper dependencies accumulated while generating every
/// function, merged the same way by both the single-file and the split driver
/// (so the two cannot drift). Rendering the module/root consumes these to emit
/// exactly the helpers, dispatch methods and runtime types the bodies used.
#[derive(Default)]
pub(crate) struct ModuleDeps {
    pub(crate) helpers: HashSet<(Helper, u32)>,
    pub(crate) rt: HashSet<Rt>,
    pub(crate) simd: HashSet<&'static str>,
    pub(crate) dispatch_sigs: HashSet<u32>,
    pub(crate) uses_eh: bool,
    /// Shared state structs from split flattened methods, emitted at the crate
    /// root (see [`GenMeta::state_structs`]).
    pub(crate) state_structs: Vec<String>,
}
impl ModuleDeps {
    /// Fold one function's discovered dependencies into the running totals.
    fn merge(&mut self, meta: GenMeta) {
        self.helpers.extend(meta.helpers);
        self.rt.extend(meta.rt);
        self.simd.extend(meta.simd);
        self.dispatch_sigs.extend(meta.dispatch_sigs);
        self.uses_eh |= meta.uses_eh;
        self.state_structs.extend(meta.state_structs);
    }
}
/// Translate a whole module into a single Rust source string.
///
/// A module that declares linear memory, a table or globals carries mutable
/// state, so it is emitted as a `pub struct Instance` with the functions as
/// `&mut self` methods. A stateless module keeps its functions as free
/// `pub fn`s, matching the earlier phases exactly.
pub(crate) fn generate_module(parts: &ModuleParts<'_>) -> Result<String, TranspileError> {
    let (ctx, stateful) = build_ctx(parts)?;

    // Every function is rendered back to back into one buffer; `ends` marks
    // where each one stops so the pieces can be re-sliced without per-function
    // allocations. A stateful module's methods sit inside `impl Instance`, so
    // they are indented as they are generated (a blank line before each, as in
    // the split output) and the buffer then becomes the module body as is.
    let line_prefix = if stateful { "    " } else { "" };
    let mut funcs_buf = String::new();
    let mut ends = Vec::with_capacity(parts.funcs.len());
    let mut deps = ModuleDeps::default();
    for (index, f) in parts.funcs.iter().enumerate() {
        if !parts.live[index] {
            continue;
        }
        if stateful {
            funcs_buf.push('\n');
        }
        // Defined functions are named by their full function index, i.e. after
        // the imported functions in the shared index space.
        let meta = generate_function_into(
            parts.imports.len() + index,
            f,
            &ctx,
            line_prefix,
            &mut funcs_buf,
        )?;
        deps.merge(meta);
        ends.push(funcs_buf.len());
    }
    // Every rendered dispatch method (`call_ref_t{ti}`) carries the type-mismatch
    // trap, so its cold helper must exist whenever any dispatch method is emitted.
    if !deps.dispatch_sigs.is_empty() {
        deps.rt.insert(Rt::TrapIndirectTypeMismatch);
    }

    // Free-function runtime helpers live at module scope, above the functions
    // (or the `struct Instance`) that call them, in both module shapes.
    let rt_helpers = render_rt_helpers(&deps.rt);
    let simd_helpers = render_simd_helpers(&deps.simd);

    // The exception type, then the runtime helpers, precede the module body.
    let mut out = String::new();
    if deps.uses_eh {
        out.push_str(EXC_DEF);
        out.push('\n');
    }
    if needs_gc_types(parts)? {
        out.push_str(GCREF_DEF);
        out.push('\n');
    }
    // Shared state structs from split flattened methods, at module scope ahead of
    // the `struct Instance`/functions that reference them (a field may be a
    // `GcRef`, so after its definition above).
    for state_struct in &deps.state_structs {
        out.push_str(state_struct);
        out.push('\n');
    }
    out.push_str(&rt_helpers);
    out.push_str(&simd_helpers);
    if !out.is_empty() {
        out.push('\n');
    }

    if !stateful {
        // The functions are written straight after the prelude: no separate
        // body string and no final `prelude + body` copy.
        let sources = split_at_ends(&funcs_buf, &ends);
        out.reserve(funcs_buf.len() + sources.len());
        for (i, src) in sources.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            out.push_str(src);
        }
        for line in render::entry_point_lines(parts, &ctx)? {
            out.push('\n');
            out.push_str(&line);
        }
        Ok(out)
    } else {
        // Everything before the methods goes in front of the already-rendered
        // method bodies and the tail after them, in place: the bodies (the bulk
        // of the output) are never copied into a second module-sized buffer.
        render::render_module_head(&mut out, parts, &ctx, &deps.helpers, &deps.dispatch_sigs)?;
        let mut tail = String::new();
        render::render_module_tail(&mut tail, parts, &ctx, &deps.dispatch_sigs)?;
        funcs_buf.reserve(out.len() + tail.len());
        funcs_buf.insert_str(0, &out);
        funcs_buf.push_str(&tail);
        Ok(funcs_buf)
    }
}

/// Slice `buf` into consecutive pieces ending at each offset in `ends`.
fn split_at_ends<'a>(buf: &'a str, ends: &[usize]) -> Vec<&'a str> {
    let mut start = 0;
    ends.iter()
        .map(|&end| {
            let piece = &buf[start..end];
            start = end;
            piece
        })
        .collect()
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
    let mut deps = ModuleDeps::default();

    // A stateful chunk wraps its functions in an `impl Instance` block, so every
    // emitted line is indented one level; a stateless chunk emits free `pub fn`s
    // at column zero.
    let line_prefix = if stateful { "    " } else { "" };
    // The current chunk is built in place: its prelude is written when the first
    // function joins it, each function's source is streamed straight in (never
    // buffered as a separate `String`), and the whole buffer is handed to `emit`
    // and reset at a flush. Peak memory is therefore about one chunk, not the
    // whole program.
    let mut chunk = String::new();
    let mut funcs_in_chunk = 0usize;
    let mut chunk_index = 0usize;
    for (index, f) in parts.funcs.iter().enumerate() {
        if !parts.live[index] {
            continue;
        }
        if funcs_in_chunk == 0 {
            chunk.push_str(&chunk_prelude(parts, stateful));
        }
        // A blank line separates each function from the prelude or its
        // predecessor, matching the single-file rendering.
        chunk.push('\n');
        let meta = generate_function_into(base + index, f, &ctx, line_prefix, &mut chunk)?;
        deps.merge(meta);
        funcs_in_chunk += 1;

        // Flush at the function count cap or once the chunk's own bytes reach
        // the byte cap (both act only here, at a function boundary).
        if funcs_in_chunk >= per || chunk.len() >= byte_cap {
            if stateful {
                chunk.push_str("}\n");
            }
            emit(
                format!("funcs_{chunk_index}.rs"),
                std::mem::take(&mut chunk),
            )?;
            chunk_index += 1;
            funcs_in_chunk = 0;
        }
    }
    if funcs_in_chunk > 0 {
        if stateful {
            chunk.push_str("}\n");
        }
        emit(
            format!("funcs_{chunk_index}.rs"),
            std::mem::take(&mut chunk),
        )?;
        chunk_index += 1;
    }
    let n_chunks = chunk_index;
    // Every rendered dispatch method (`call_ref_t{ti}`) carries the type-mismatch
    // trap, so its cold helper must exist whenever any dispatch method is emitted.
    if !deps.dispatch_sigs.is_empty() {
        deps.rt.insert(Rt::TrapIndirectTypeMismatch);
    }

    // The root is emitted last, once the used-helper/dispatch sets are complete.
    let root_deps = render::RootDeps {
        helpers: &deps.helpers,
        rt: &deps.rt,
        simd: &deps.simd,
        dispatch_sigs: &deps.dispatch_sigs,
    };
    let mut root = render_lib_root(parts, &ctx, stateful, &root_deps, n_chunks)?;
    // Everything that must sit above the root, outermost first, assembled once
    // and placed in front in a single insertion (the root carries the data
    // statics, so re-formatting it per piece would copy it each time):
    //  - the exception type and the managed value model, so every chunk's
    //    `use super::*` sees them;
    //  - the shared state structs from split flattened methods (an `impl` block
    //    cannot hold a struct), after `GcRef` since a field may be one, and
    //    ahead of the `impl` that references them.
    let mut prefix = String::new();
    if deps.uses_eh {
        prefix.push_str(EXC_DEF);
        prefix.push('\n');
    }
    if needs_gc_types(parts)? {
        prefix.push_str(GCREF_DEF);
        prefix.push('\n');
    }
    if !deps.state_structs.is_empty() {
        prefix.push_str(&deps.state_structs.join("\n"));
        prefix.push('\n');
    }
    root.insert_str(0, &prefix);
    emit("lib.rs".to_string(), root)
}
/// Generate the Rust source of one function, appending it to `out` (each
/// non-empty line prefixed by `line_prefix`, used to indent a method inside a
/// chunk's `impl` block), and return the helper dependencies it discovered.
pub(crate) fn generate_function_into(
    index: usize,
    input: &FuncInput<'_>,
    ctx: &ModuleCtx<'_>,
    line_prefix: &str,
    out: &mut String,
) -> Result<GenMeta, TranspileError> {
    let mut func = FuncGen::new(input.params, input.results, input.body, ctx)?;
    // A step function (a continuation body, or a function reached from one
    // through a suspend-crossing call) is emitted as a resumable
    // `cont_step_func{N}` state machine instead of an ordinary `func{N}`.
    if ctx.cont.step_set.binary_search(&index_u32(index)?).is_ok() {
        return func.emit_cont_step(index, input.params, input.body, line_prefix, out);
    }
    func.run(input.body)?;
    func.finish(index, input.params, input.results, line_prefix, out)
}

/// Function indices keyed by exact `(params, results)` signature.
type SigIndex = HashMap<(Vec<ValType>, Vec<ValType>), Vec<usize>>;

/// Groups function indices (in iteration order, so ascending) by their exact
/// `(params, results)` signature.
fn index_by_signature<'a>(
    sigs: impl IntoIterator<Item = Option<(&'a [ValType], &'a [ValType])>>,
) -> SigIndex {
    let mut index = SigIndex::new();
    // A `None` entry (a dead function) keeps its slot in the index space but is
    // never a dispatch target.
    for (fidx, sig) in sigs.into_iter().enumerate() {
        let Some((params, results)) = sig else {
            continue;
        };
        index
            .entry((params.to_vec(), results.to_vec()))
            .or_default()
            .push(fidx);
    }
    index
}

/// For every type index, the concrete struct/array types whose declared
/// supertype chain reaches it (a type reaches itself), each list ascending.
///
/// Each candidate walks its own chain once and registers itself with every
/// ancestor, so the whole table costs O(types x chain depth) instead of a
/// full-hierarchy scan per query. The walk is bounded by the type count, so a
/// cycle (which a validated module cannot contain) still terminates; the
/// `last()` check keeps a cycle from registering a type twice.
fn index_descendants(type_kinds: &[CompositeKind], supers: &[Option<u32>]) -> Vec<Vec<u32>> {
    let n = type_kinds.len();
    let mut index = vec![Vec::new(); n];
    for (i, kind) in type_kinds.iter().enumerate() {
        // A candidate must be a concrete struct/array type with a
        // representable index (a valid module never overflows `u32`).
        if !matches!(kind, CompositeKind::Struct(_) | CompositeKind::Array(_)) {
            continue;
        }
        let Ok(t) = u32::try_from(i) else { continue };
        let mut cur = Some(t);
        for _ in 0..=n {
            let Some(c) = cur else { break };
            if let Some(list) = index.get_mut(c as usize)
                && list.last() != Some(&t)
            {
                list.push(t);
            }
            cur = supers.get(c as usize).copied().flatten();
        }
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_by_signature_groups_function_indices_in_ascending_order() {
        let i32_to_i32: (&[ValType], &[ValType]) = (&[ValType::I32], &[ValType::I32]);
        let i64_to_i64: (&[ValType], &[ValType]) = (&[ValType::I64], &[ValType::I64]);
        let nullary: (&[ValType], &[ValType]) = (&[], &[]);
        let index =
            index_by_signature([i32_to_i32, i64_to_i64, i32_to_i32, nullary, i32_to_i32].map(Some));

        let key = |(p, r): (&[ValType], &[ValType])| (p.to_vec(), r.to_vec());
        assert_eq!(index.get(&key(i32_to_i32)), Some(&vec![0, 2, 4]));
        assert_eq!(index.get(&key(i64_to_i64)), Some(&vec![1]));
        assert_eq!(index.get(&key(nullary)), Some(&vec![3]));
        assert_eq!(index.get(&key((&[ValType::F32], &[]))), None);
    }

    /// The pre-index algorithm: scan every type and walk its super chain.
    fn descendants_by_scan(
        type_kinds: &[CompositeKind],
        supers: &[Option<u32>],
        target: u32,
    ) -> Vec<u32> {
        let n = type_kinds.len();
        (0u32..)
            .take(n)
            .filter(|&t| {
                matches!(
                    type_kinds[t as usize],
                    CompositeKind::Struct(_) | CompositeKind::Array(_)
                )
            })
            .filter(|&t| {
                let mut cur = Some(t);
                for _ in 0..=n {
                    match cur {
                        Some(c) if c == target => return true,
                        Some(c) => cur = supers.get(c as usize).copied().flatten(),
                        None => return false,
                    }
                }
                false
            })
            .collect()
    }

    fn assert_matches_scan(kinds: &[CompositeKind], supers: &[Option<u32>]) {
        let index = index_descendants(kinds, supers);
        assert_eq!(index.len(), kinds.len());
        for target in (0u32..).take(kinds.len()) {
            assert_eq!(
                index[target as usize],
                descendants_by_scan(kinds, supers, target),
                "descendants of type {target}"
            );
        }
    }

    fn array() -> CompositeKind {
        CompositeKind::Array(FieldInfo {
            storage: wasmparser::StorageType::Val(ValType::I32),
        })
    }

    #[test]
    fn index_descendants_matches_a_scan_of_every_type() {
        // 0 <- 1 <- 2 (chain), 0 <- 3 (sibling), 4 func (abstract root of 5),
        // 5 struct under a func type, 6 lone struct, 7 cont, 8 array under 0,
        // 9 lone array.
        let kinds = [
            CompositeKind::Struct(vec![]),
            CompositeKind::Struct(vec![]),
            CompositeKind::Struct(vec![]),
            CompositeKind::Struct(vec![]),
            CompositeKind::Func,
            CompositeKind::Struct(vec![]),
            CompositeKind::Struct(vec![]),
            CompositeKind::Cont(4),
            array(),
            array(),
        ];
        let supers = [
            None,
            Some(0),
            Some(1),
            Some(0),
            None,
            Some(4),
            None,
            None,
            Some(0),
            None,
        ];
        assert_matches_scan(&kinds, &supers);
        assert_eq!(index_descendants(&kinds, &supers)[0], vec![0, 1, 2, 3, 8]);
    }

    #[test]
    fn index_descendants_terminates_and_dedups_on_a_cycle() {
        // A validated module has no cyclic super chain, but the walk is hop
        // bounded so malformed input must still terminate with each type
        // registered once per ancestor.
        let kinds = [CompositeKind::Struct(vec![]), CompositeKind::Struct(vec![])];
        let supers = [Some(1), Some(0)];
        assert_matches_scan(&kinds, &supers);
        assert_eq!(index_descendants(&kinds, &supers)[0], vec![0, 1]);
    }
}
