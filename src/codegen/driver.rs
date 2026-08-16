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
    /// Every function type, so `call_indirect` can resolve its declared type
    /// index back to a signature.
    pub(crate) types: &'a [TypeSig],
    /// The kind (function / struct / array) of every type index, so a concrete
    /// reference and the struct/array operators can resolve field/element
    /// storage and pick the right value lowering.
    pub(crate) type_kinds: &'a [CompositeKind],
    /// The declared direct supertype of every type index (`None` when a type
    /// declares no supertype), parallel to `type_kinds`. A runtime downcast
    /// check (`ref.test`/`ref.cast`/`br_on_cast`) walks this chain to compute
    /// the set of concrete subtypes of a static target.
    pub(crate) supers: &'a [Option<u32>],
    /// Per-imported-global `(type, mutable)`, occupying the low global indices.
    pub(crate) imported_globals: Vec<(ValType, bool)>,
    /// Per-defined-global `(type, mutable)`, indexed after the imported globals.
    pub(crate) globals: Vec<(ValType, bool)>,
    /// Whether the module declares linear memory (so `self.mem()` exists).
    pub(crate) has_memory: bool,
    /// The number of linear memories (imported first, then defined). Memory
    /// operations carry a static index that must be below this.
    pub(crate) n_memories: usize,
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
    /// reaches `target` (including `target` itself when it is a struct/array).
    ///
    /// The whole type hierarchy is known statically, so a runtime downcast to a
    /// static `target` reduces to membership in this set of concrete type ids.
    /// The chain is walked per candidate; a cycle (which a validated module
    /// cannot contain) is guarded by a hop budget so the walk always terminates.
    pub(crate) fn concrete_descendants(&self, target: u32) -> Vec<u32> {
        let n = self.type_kinds.len();
        self.type_kinds
            .iter()
            .enumerate()
            // A candidate must be a concrete struct/array type with a
            // representable index (a valid module never overflows `u32`).
            .filter(|(_, kind)| matches!(kind, CompositeKind::Struct(_) | CompositeKind::Array(_)))
            .filter_map(|(i, _)| index_u32(i).ok())
            .filter(|&t| {
                // Walk `t`'s super chain, bounded by the type count, looking for
                // `target` (a type is trivially a subtype of itself).
                let mut cur = Some(t);
                for _ in 0..=n {
                    match cur {
                        Some(c) if c == target => return true,
                        Some(c) => cur = self.supers.get(c as usize).copied().flatten(),
                        None => return false,
                    }
                }
                false
            })
            .collect()
    }

    /// The number of functions in the shared index space (imports then defined),
    /// i.e. the range of valid full indices.
    pub(crate) fn func_count(&self) -> usize {
        self.imports.len() + self.funcs.len()
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
    // The host is injected whenever anything is imported (globals, non-WASI
    // functions, or host-owned memory/table).
    let has_imports = !imports.is_empty()
        || !imported_globals.is_empty()
        || memories.iter().any(|m| m.imported)
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
        || !cont_bodies.is_empty();

    let ctx = ModuleCtx {
        imports,
        funcs,
        types,
        type_kinds,
        supers,
        imported_globals: imported_globals.iter().map(|g| (g.ty, g.mutable)).collect(),
        globals: globals.iter().map(|g| (g.ty, g.mutable)).collect(),
        has_memory,
        n_memories: memories.len(),
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

    let mut sources = Vec::with_capacity(parts.funcs.len());
    let mut deps = ModuleDeps::default();
    for (index, f) in parts.funcs.iter().enumerate() {
        // Defined functions are named by their full function index, i.e. after
        // the imported functions in the shared index space. The single-file
        // path is not memory-critical, so each function is rendered into its own
        // `String` and the pieces are joined/wrapped exactly as before.
        let mut src = String::new();
        let meta = generate_function_into(parts.imports.len() + index, f, &ctx, "", &mut src)?;
        deps.merge(meta);
        sources.push(src);
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

    let body = if !stateful {
        sources.join("\n")
    } else {
        render_module(parts, &ctx, &sources, &deps.helpers, &deps.dispatch_sigs)?
    };

    // The exception type, then the runtime helpers, precede the module body.
    let mut prelude = String::new();
    if deps.uses_eh {
        prelude.push_str(EXC_DEF);
        prelude.push('\n');
    }
    if needs_gc_types(parts)? {
        prelude.push_str(GCREF_DEF);
        prelude.push('\n');
    }
    // Shared state structs from split flattened methods, at module scope ahead of
    // the `struct Instance`/functions that reference them (a field may be a
    // `GcRef`, so after its definition above).
    for state_struct in &deps.state_structs {
        prelude.push_str(state_struct);
        prelude.push('\n');
    }
    prelude.push_str(&rt_helpers);
    prelude.push_str(&simd_helpers);
    Ok(if prelude.is_empty() {
        body
    } else {
        format!("{prelude}\n{body}")
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
    let root = render_lib_root(parts, &ctx, stateful, &root_deps, n_chunks)?;
    // Shared state structs from split flattened methods live at the crate root
    // (an `impl` block cannot hold a struct), ahead of the `impl` that references
    // them. Placed here so the later `GcRef`/exception prepends stay above them —
    // a struct field may be a `GcRef`, which must already be in scope.
    let root = if deps.state_structs.is_empty() {
        root
    } else {
        format!("{}\n{root}", deps.state_structs.join("\n"))
    };
    // The exception type and the managed value model live at the crate root so
    // every chunk's `use super::*` sees them, ahead of everything else in
    // `lib.rs`.
    let root = if needs_gc_types(parts)? {
        format!("{GCREF_DEF}\n{root}")
    } else {
        root
    };
    let root = if deps.uses_eh {
        format!("{EXC_DEF}\n{root}")
    } else {
        root
    };
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
