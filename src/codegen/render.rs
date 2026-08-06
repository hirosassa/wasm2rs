use std::collections::HashSet;

use super::helpers::{HELPER_ORDER, helper_lines, shared_helper_lines};
use super::runtime::render_rt_helpers;
use super::{
    ALLOW, DataSegment, ElemSegment, Helper, ImportInfo, ImportedGlobalInfo, ModuleCtx,
    ModuleParts, WasiFn, byte_array_literal, default_value, indent, index_u32, rust_type,
    rust_types,
};
use crate::TranspileError;

/// Render the `struct Instance` and its `impl` for a stateful module. When the
/// module imports functions, a `pub trait Imports` is emitted and `Instance`
/// becomes generic over a host `H: Imports` that it stores and dispatches to.
pub(super) fn render_module(
    parts: &ModuleParts<'_>,
    ctx: &ModuleCtx<'_>,
    sources: &[String],
    used: &HashSet<(Helper, u32)>,
    dispatch_sigs: &HashSet<u32>,
) -> Result<String, TranspileError> {
    let ModuleParts {
        imports,
        imported_globals,
        globals,
        memories,
        data,
        table,
        elements,
        ..
    } = *parts;

    // Defined globals are named by their full index, i.e. after the imported
    // globals in the shared global index space.
    let global_base = imported_globals.len();

    let mut lines: Vec<String> = Vec::new();

    // Only memory 0 may be imported (an imported non-zero memory is rejected
    // during parsing); `mem_imported` therefore governs just memory 0's storage.
    let mem_imported = memories.first().is_some_and(|m| m.imported);
    let table_imported = table.is_some_and(|t| t.imported);
    let has_imports = needs_host_trait(parts);
    // A module that imports any preopen/`path_open` function gains a real
    // file-descriptor table (`wasi_fds`) and the file-backed fd_* variants.
    let wasi_files = imports.iter().any(|im| {
        matches!(
            im.wasi,
            Some(
                WasiFn::PathOpen
                    | WasiFn::FdPrestatGet
                    | WasiFn::FdPrestatDirName
                    | WasiFn::FdFilestatGet
                    | WasiFn::FdReaddir
            )
        )
    });
    if has_imports {
        lines.extend(import_trait_lines(
            ctx,
            imports,
            imported_globals,
            mem_imported,
            table_imported,
            dispatch_sigs,
        )?);
        lines.push(String::new());
    }
    // A shared module emits the thread-shareable `SharedMemory` runtime once,
    // at module scope, ahead of the `Instance` type that stores it.
    if ctx.memory_shared {
        lines.extend(shared_memory_runtime_lines());
        lines.push(String::new());
    }
    let (decl_generics, type_generics) = host_generics(parts);

    // Module-scope statics backing the retained passive segments.
    for (d, seg) in data.iter().enumerate() {
        if seg.offset.is_none() {
            let bytes_lit = byte_array_literal(&seg.bytes);
            lines.push(format!(
                "static DATA{d}: [u8; {}] = [{bytes_lit}];",
                seg.bytes.len()
            ));
        }
    }
    for (e, seg) in elements.iter().enumerate() {
        if seg.offset.is_none() && !seg.declared {
            let funcs_lit = seg
                .funcs
                .iter()
                .map(|f| format!("{f}u32"))
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!(
                "static ELEM{e}: [u32; {}] = [{funcs_lit}];",
                seg.funcs.len()
            ));
        }
    }

    // Typed-continuations runtime: the step result, the per-continuation frame
    // structs and the tagged object stored in the instance's `conts` table.
    if !ctx.cont_bodies.is_empty() {
        lines.extend(continuation_runtime_lines(ctx)?);
        lines.push(String::new());
    }

    lines.push("#[allow(dead_code)]".to_string());
    lines.push(format!("pub struct Instance{decl_generics} {{"));
    if has_imports {
        lines.push("    imports: H,".to_string());
    }
    // A single defined `shared` memory is a thread-shareable handle (cheap Arc
    // clone) rather than an owned `Vec<u8>`, so sibling instances on other
    // threads share the same linear memory.
    if ctx.memory_shared {
        lines.push("    memory: SharedMemory,".to_string());
    } else {
        // Each defined memory is an owned buffer: memory 0 keeps the field name
        // `memory`, memory `i > 0` is `memory{i}`. An imported memory (only ever
        // memory 0) lives in the host, so the instance owns no buffer for it.
        for (i, _) in memories.iter().enumerate() {
            if i == 0 && mem_imported {
                continue;
            }
            let field = if i == 0 {
                "memory".to_string()
            } else {
                format!("memory{i}")
            };
            lines.push(format!("    {field}: Vec<u8>,"));
        }
    }
    // Imported tables live in the host, so the instance owns no storage.
    if table.is_some() && !table_imported {
        // A table entry is a function index; `u32::MAX` marks a null funcref.
        lines.push("    table: Vec<u32>,".to_string());
    }
    for (i, g) in globals.iter().enumerate() {
        lines.push(format!(
            "    g{}: {},",
            global_base + i,
            rust_type(g.ty, ctx.type_kinds)?
        ));
    }
    // Files/directories opened through `path_open`; descriptor N is
    // `wasi_fds[N - 4]` (0-2 are stdio, 3 is the preopen directory). Each slot
    // keeps the open handle and its containment-checked relative path (the path
    // is what `fd_readdir` re-opens with `read_dir`, since a `File` cannot be
    // enumerated directly with std alone).
    if wasi_files {
        lines.push("    wasi_fds: Vec<Option<(std::fs::File, std::path::PathBuf)>>,".to_string());
    }
    // A retained passive segment is a `&'static` slice; `data.drop`/`elem.drop`
    // reset it to an empty slice.
    for (d, seg) in data.iter().enumerate() {
        if seg.offset.is_none() {
            lines.push(format!("    data{d}: &'static [u8],"));
        }
    }
    for (e, seg) in elements.iter().enumerate() {
        if seg.offset.is_none() && !seg.declared {
            lines.push(format!("    elem{e}: &'static [u32],"));
        }
    }
    // The externref internalisation box: `extern.convert_any` pushes a managed
    // handle here and returns its index as the `u32` externref.
    if ctx.uses_extern_box {
        lines.push("    extern_box: Vec<GcRef>,".to_string());
    }
    // The continuation handle table: a `cont.new` pushes a fresh resumable
    // object here and returns its index; `None` marks a slot whose continuation
    // has run to completion (a one-shot continuation, consumed on return).
    if !ctx.cont_bodies.is_empty() {
        lines.push("    conts: Vec<Option<ContObj>>,".to_string());
    }
    lines.push("}".to_string());
    lines.push(String::new());

    lines.push(ALLOW.to_string());
    lines.push(format!("impl{decl_generics} Instance{type_generics} {{"));

    // Everything inside the `impl` is collected unindented, then indented once.
    let mut inner: Vec<String> = Vec::new();
    let new_param = if has_imports { "imports: H" } else { "" };
    // Only active segments are copied at instantiation; passive ones are
    // retained (see the `data{d}` fields) for `memory.init`. Each active data
    // segment names the memory it initialises.
    let active: Vec<&DataSegment> = data.iter().filter(|d| d.offset.is_some()).collect();
    let active_elem: Vec<&ElemSegment> = elements.iter().filter(|e| e.offset.is_some()).collect();
    // The active data segments that target a given memory index, in order.
    let active_for =
        |mem: u32| -> Vec<&&DataSegment> { active.iter().filter(|d| d.mem_index == mem).collect() };
    // Imported memory/table are host-owned, so their active data/elements cannot
    // be written into a `memory`/`table` field literal; instead the instance is
    // bound and the segments are copied into the host storage through
    // `mem_mut()`/`table_mut()` after construction. Only memory 0 can be
    // imported, so only its segments need post-construction copying.
    let post_init_data = mem_imported && !active_for(0).is_empty();
    let post_init_elem = table_imported && !active_elem.is_empty();
    let post_init = post_init_data || post_init_elem;
    let (open, close) = if post_init {
        ("    let mut instance = Self {", "    };")
    } else {
        ("    Self {", "    }")
    };

    // Emit `<target>[off..end].copy_from_slice(&[bytes]);` for each active data
    // segment targeting memory `mem`; `target` is a defined buffer (`m`) or the
    // host buffer accessor (`instance.mem_mut()`), and `indent` matches the
    // surrounding block.
    let copy_active_data = |lines: &mut Vec<String>, mem: u32, target: &str, indent: &str| {
        for seg in active_for(mem) {
            let off = seg.offset.unwrap_or(0) as usize;
            let end = off + seg.bytes.len();
            let bytes_lit = byte_array_literal(&seg.bytes);
            lines.push(format!(
                "{indent}{target}[{off}..{end}].copy_from_slice(&[{bytes_lit}]);"
            ));
        }
    };

    // Emit `<target>[idx] = fu32;` for each active element segment entry;
    // `target` is a defined table (`t`) or the host storage accessor
    // (`instance.table_mut()`).
    let copy_active_elems = |lines: &mut Vec<String>, target: &str, indent: &str| {
        for seg in &active_elem {
            for (k, f) in seg.funcs.iter().enumerate() {
                let idx = seg.offset.unwrap_or(0) as usize + k;
                lines.push(format!("{indent}{target}[{idx}] = {f}u32;"));
            }
        }
    };

    // The non-memory field initialisers (imports, table, globals, wasi_fds, and
    // the retained passive segments), pushed identically by both `Instance::new`
    // and — for a shared module — `with_memory`. Only the memory field differs
    // between the two, so factoring the rest keeps them in lock-step.
    let push_common_fields = |inner: &mut Vec<String>| -> Result<(), TranspileError> {
        // An imported table is host-owned (see the post-construction elem copy);
        // only a defined table gets a `table` field.
        if let Some(t) = table.filter(|_| !table_imported) {
            if active_elem.is_empty() {
                inner.push(format!("        table: vec![u32::MAX; {}],", t.min));
            } else {
                // Start every slot null, then apply each active element segment.
                inner.push("        table: {".to_string());
                inner.push(format!(
                    "            let mut t: Vec<u32> = vec![u32::MAX; {}];",
                    t.min
                ));
                copy_active_elems(inner, "t", "            ");
                inner.push("            t".to_string());
                inner.push("        },".to_string());
            }
        }
        for (i, g) in globals.iter().enumerate() {
            inner.push(format!("        g{}: {},", global_base + i, g.init));
        }
        if wasi_files {
            inner.push("        wasi_fds: Vec::new(),".to_string());
        }
        // Passive segments are retained as `&'static` slices of the module-scope
        // statics, so `memory.init`/`table.init` can copy from them on demand.
        for (d, seg) in data.iter().enumerate() {
            if seg.offset.is_none() {
                inner.push(format!("        data{d}: &DATA{d},"));
            }
        }
        for (e, seg) in elements.iter().enumerate() {
            if seg.offset.is_none() && !seg.declared {
                inner.push(format!("        elem{e}: &ELEM{e},"));
            }
        }
        if ctx.uses_extern_box {
            inner.push("        extern_box: Vec::new(),".to_string());
        }
        if !ctx.cont_bodies.is_empty() {
            inner.push("        conts: Vec::new(),".to_string());
        }
        Ok(())
    };

    if ctx.memory_shared {
        // A single defined shared memory. `new` creates a fresh `SharedMemory`,
        // applies this module's active data segments into it *once*, and inits
        // globals/tables; `with_memory` joins an existing handle (sibling
        // instance on another thread), inits its own globals/tables, and does
        // NOT re-apply data — the memory is already initialised.
        let mem0 = memories
            .first()
            .ok_or_else(|| TranspileError::Unsupported("shared memory missing".into()))?;
        let bytes = mem0
            .min_pages
            .checked_mul(65536)
            .ok_or_else(|| TranspileError::Unsupported("memory too large".into()))?;

        inner.push(format!("pub fn new({new_param}) -> Self {{"));
        inner.push(format!("    let memory = SharedMemory::new({bytes});"));
        if !active_for(0).is_empty() {
            inner.push("    {".to_string());
            inner.push("        let mut __m = memory.bytes();".to_string());
            copy_active_data(&mut inner, 0, "__m", "        ");
            inner.push("    }".to_string());
        }
        inner.push("    Self {".to_string());
        if has_imports {
            inner.push("        imports,".to_string());
        }
        inner.push("        memory,".to_string());
        push_common_fields(&mut inner)?;
        inner.push("    }".to_string());
        inner.push("}".to_string());

        let with_param = if has_imports {
            "mem: SharedMemory, imports: H"
        } else {
            "mem: SharedMemory"
        };
        inner.push(String::new());
        inner.push(format!("pub fn with_memory({with_param}) -> Self {{"));
        inner.push("    Self {".to_string());
        if has_imports {
            inner.push("        imports,".to_string());
        }
        inner.push("        memory: mem,".to_string());
        push_common_fields(&mut inner)?;
        inner.push("    }".to_string());
        inner.push("}".to_string());

        inner.push(String::new());
        inner.push(
            "pub fn shared_memory(&self) -> SharedMemory { self.memory.clone() }".to_string(),
        );
    } else {
        inner.push(format!("pub fn new({new_param}) -> Self {{"));
        inner.push(open.to_string());
        if has_imports {
            inner.push("        imports,".to_string());
        }
        // Each defined memory's field literal: zero-filled, then each active data
        // segment for that memory copied in. Memory 0 keeps the `memory` field name
        // and the exact single-memory rendering; memory `i > 0` is `memory{i}`.
        for (i, m) in memories.iter().enumerate() {
            if i == 0 && mem_imported {
                continue;
            }
            let field = if i == 0 {
                "memory".to_string()
            } else {
                format!("memory{i}")
            };
            let mem_index = index_u32(i)?;
            let bytes = m
                .min_pages
                .checked_mul(65536)
                .ok_or_else(|| TranspileError::Unsupported("memory too large".into()))?;
            if active_for(mem_index).is_empty() {
                inner.push(format!("        {field}: vec![0u8; {bytes}],"));
            } else {
                // Zero the memory, then copy each active data segment into place.
                inner.push(format!("        {field}: {{"));
                inner.push(format!(
                    "            let mut m: Vec<u8> = vec![0u8; {bytes}];"
                ));
                copy_active_data(&mut inner, mem_index, "m", "            ");
                inner.push("            m".to_string());
                inner.push("        },".to_string());
            }
        }
        push_common_fields(&mut inner)?;
        inner.push(close.to_string());
        if post_init {
            // Copy each active data/element segment into the host-owned storage in
            // order. An out-of-bounds write panics here, faithfully reproducing a
            // wasm instantiation trap when a segment does not fit the host storage.
            if post_init_data {
                copy_active_data(&mut inner, 0, "instance.mem_mut()", "    ");
            }
            if post_init_elem {
                copy_active_elems(&mut inner, "instance.table_mut()", "    ");
            }
            inner.push("    instance".to_string());
        }
        inner.push("}".to_string());
    }

    // Uniform memory accessors so the load/store/bulk helpers are identical for
    // defined and imported memory: a defined buffer is a field, an imported one
    // (only ever memory 0) is lent by the host through the `Imports` trait.
    // Memory 0 keeps the historic `mem`/`mem_mut`/`memory` names; memory `i > 0`
    // is `mem{i}`/`mem{i}_mut`/`memory{i}`, each backed by its own field.
    //
    // A shared module emits none of these: `mem()`/`mem_mut()` hand out plain
    // borrows that cannot span a `Mutex` lock, and the transformed helpers lock
    // `self.memory.bytes()` directly instead. `shared_memory()` (emitted with
    // `new`) is the public handle accessor.
    for (i, m) in memories.iter().enumerate().filter(|_| !ctx.memory_shared) {
        let (get, get_mut, pub_get) = if i == 0 {
            (
                "mem".to_string(),
                "mem_mut".to_string(),
                "memory".to_string(),
            )
        } else {
            (
                format!("mem{i}"),
                format!("mem{i}_mut"),
                format!("memory{i}"),
            )
        };
        let (borrow, borrow_mut) = if i == 0 && m.imported {
            (
                "self.imports.memory()".to_string(),
                "self.imports.memory_mut()".to_string(),
            )
        } else {
            let field = if i == 0 {
                "memory".to_string()
            } else {
                format!("memory{i}")
            };
            (format!("&self.{field}"), format!("&mut self.{field}"))
        };
        inner.push(String::new());
        inner.push(format!("fn {get}(&self) -> &[u8] {{ {borrow} }}"));
        inner.push(format!(
            "fn {get_mut}(&mut self) -> &mut Vec<u8> {{ {borrow_mut} }}"
        ));
        // Public accessor so a host embedding this `Instance` can marshal bytes
        // into and out of linear memory (e.g. write an RPC request buffer and
        // read the response). `&mut` covers both reads and writes since a host
        // driving the module already holds it mutably.
        inner.push(format!(
            "pub fn {pub_get}(&mut self) -> &mut Vec<u8> {{ {borrow_mut} }}"
        ));
    }

    // Uniform table accessors, mirroring the memory ones: a defined table is a
    // field, an imported one is lent by the host through the `Imports` trait.
    if let Some(t) = table {
        let (borrow, borrow_mut) = if t.imported {
            ("self.imports.table()", "self.imports.table_mut()")
        } else {
            ("&self.table", "&mut self.table")
        };
        inner.push(String::new());
        inner.push(format!("fn table(&self) -> &[u32] {{ {borrow} }}"));
        inner.push(format!(
            "fn table_mut(&mut self) -> &mut Vec<u32> {{ {borrow_mut} }}"
        ));
    }

    // Native WASI functions are inherent methods backed by `self.mem()`; emit
    // each recognised kind once, in first-import order.
    let mut wasi_emitted: Vec<WasiFn> = Vec::new();
    for im in imports {
        if let Some(w) = im.wasi
            && !wasi_emitted.contains(&w)
        {
            wasi_emitted.push(w);
            inner.push(String::new());
            inner.extend(w.lines(wasi_files));
        }
    }

    // Emit each used helper method, grouped by memory index (0 first, so the
    // historic single-memory helpers keep their position) then in the canonical
    // HELPER_ORDER within each memory.
    let mut mem_indices: Vec<u32> = used.iter().map(|(_, mem)| *mem).collect();
    mem_indices.sort_unstable();
    mem_indices.dedup();
    for mem in mem_indices {
        for helper in HELPER_ORDER {
            if used.contains(&(helper, mem)) {
                inner.push(String::new());
                // A shared module (single memory 0) emits helpers that lock
                // `self.memory.bytes()` once instead of borrowing `self.mem()`.
                if ctx.memory_shared {
                    inner.extend(shared_helper_lines(helper));
                } else {
                    inner.extend(helper_lines(helper, mem));
                }
            }
        }
    }

    // One public `call_ref_t{ti}` dispatch method per `call_indirect` signature.
    // It is both the target of the module's own `call_indirect` and the entry
    // point through which the host invokes a funcref the module handed out.
    let mut dispatch_ordered: Vec<u32> = dispatch_sigs.iter().copied().collect();
    dispatch_ordered.sort_unstable();
    for ti in dispatch_ordered {
        inner.push(String::new());
        inner.extend(dispatch_method_lines(ctx, ti, has_imports)?);
    }

    // The continuation allocator (`cont.new`) and stepper (`resume`).
    if !ctx.cont_bodies.is_empty() {
        inner.push(String::new());
        inner.extend(continuation_method_lines(ctx));
    }

    for src in sources {
        inner.push(String::new());
        for line in src.lines() {
            inner.push(line.to_string());
        }
    }

    for line in indent(&inner) {
        lines.push(line);
    }
    lines.push("}".to_string());

    // Module-scope external-funcref registries (the Linker helper), one per
    // `call_indirect` signature, for modules that can receive external handles.
    if has_imports && !dispatch_sigs.is_empty() {
        lines.extend(extern_registry_lines(ctx, dispatch_sigs)?);
    }

    let mut out = lines.join("\n");
    out.push('\n');
    Ok(out)
}

/// The module-scope typed-continuations runtime: the `StepResult` a step
/// function returns, one `ContFrame{N}` per continuation-bodied function
/// (holding its resumable program counter), and the `ContObj` union the handle
/// table stores. Phase 4 continuation bodies have no parameters or locals, so a
/// frame is just its `pc`.
fn continuation_runtime_lines(ctx: &ModuleCtx<'_>) -> Result<Vec<String>, TranspileError> {
    // A program may create a continuation without resuming it (or vice versa),
    // leaving some fields/variants unexercised, so the generated types carry the
    // same dead-code allowance as the `Instance` struct.
    // The types are `pub` because the `pub` `cont_new`/`cont_step`/`cont_step_func`
    // methods (public so the root impl can reach a chunk's step function) mention
    // them in their signatures.
    let mut lines = vec![
        "#[allow(dead_code)]".to_string(),
        "pub enum StepResult {".to_string(),
        "    Return(Vec<i64>),".to_string(),
        "    Suspend { tag: u32, payload: Vec<i64> },".to_string(),
        "}".to_string(),
        String::new(),
    ];
    for n in &ctx.step_set {
        // A `ContFrame` holds the resumable `pc`, one field per local (so locals
        // survive suspends), and — when the function ends in a cross-call
        // checkpoint — the callee's frame nested as `sub`.
        let mut fields = vec!["pc: u32".to_string()];
        if let Some(g) = ctx.checkpoint_callee.get(n) {
            fields.push(format!("sub: ContFrame{g}"));
        }
        if let Some(locals) = ctx.step_locals.get(n) {
            for (i, ty) in locals.iter().enumerate() {
                fields.push(format!("l{i}: {}", rust_type(*ty, ctx.type_kinds)?));
            }
        }
        lines.push("#[allow(dead_code)]".to_string());
        lines.push(format!(
            "pub struct ContFrame{n} {{ {} }}",
            fields.join(", ")
        ));
    }
    lines.push(String::new());
    lines.push("#[allow(dead_code)]".to_string());
    lines.push("pub enum ContObj {".to_string());
    for n in &ctx.cont_bodies {
        lines.push(format!("    C{n}(ContFrame{n}),"));
    }
    lines.push("}".to_string());
    Ok(lines)
}

/// The start-state literal for a step function's frame: `pc` 0, each local at
/// its default, and — when the function ends in a cross-call checkpoint — a
/// freshly-started callee frame in `sub` (recursively). The checkpoint graph is
/// acyclic (a recursive chain is rejected during context building), so this
/// recursion terminates.
fn frame_start_literal(ctx: &ModuleCtx<'_>, n: u32) -> String {
    let mut fields = vec!["pc: 0u32".to_string()];
    if let Some(g) = ctx.checkpoint_callee.get(&n) {
        fields.push(format!("sub: {}", frame_start_literal(ctx, *g)));
    }
    if let Some(locals) = ctx.step_locals.get(&n) {
        for (i, ty) in locals.iter().enumerate() {
            fields.push(format!("l{i}: {}", default_value(*ty, ctx.type_kinds)));
        }
    }
    format!("ContFrame{n} {{ {} }}", fields.join(", "))
}

/// The `cont_new`/`cont_step` methods on `Instance`. `cont_new` allocates a
/// fresh resumable frame for the referenced function and returns its handle
/// (an index into `conts`); `cont_step` resumes the handle once, dispatching to
/// the right step function and — since a continuation is one-shot — keeping the
/// (advanced) object only while it is still suspended, dropping it on return.
fn continuation_method_lines(ctx: &ModuleCtx<'_>) -> Vec<String> {
    let mut lines = vec![
        "pub fn cont_new(&mut self, __funcidx: u32) -> u32 {".to_string(),
        "    let __obj = match __funcidx {".to_string(),
    ];
    for n in &ctx.cont_bodies {
        lines.push(format!(
            "        {n}u32 => ContObj::C{n}({}),",
            frame_start_literal(ctx, *n)
        ));
    }
    lines.extend([
        "        _ => panic!(\"cont.new: not a continuation function\"),".to_string(),
        "    };".to_string(),
        "    self.conts.push(Some(__obj));".to_string(),
        "    (self.conts.len() - 1) as u32".to_string(),
        "}".to_string(),
        String::new(),
        "pub fn cont_step(&mut self, __h: u32) -> StepResult {".to_string(),
        "    let mut __obj = self.conts[__h as usize]".to_string(),
        "        .take()".to_string(),
        "        .expect(\"resume of a consumed continuation\");".to_string(),
        "    let __r = match &mut __obj {".to_string(),
    ]);
    for n in &ctx.cont_bodies {
        lines.push(format!(
            "        ContObj::C{n}(__f) => self.cont_step_func{n}(__f),"
        ));
    }
    lines.extend([
        "    };".to_string(),
        "    if let StepResult::Suspend { .. } = __r {".to_string(),
        "        self.conts[__h as usize] = Some(__obj);".to_string(),
        "    }".to_string(),
        "    __r".to_string(),
        "}".to_string(),
    ]);
    lines
}

/// The module-scope runtime for a `shared` memory: the thread-shareable
/// `SharedMemory` handle (a `#[derive(Clone)]` `Arc` of a `Mutex`-bearing inner,
/// so it is `Send + Sync`), the little-endian read/write/combine helpers, and
/// the atomic RMW/cmpxchg/wait/notify methods. Emitted verbatim (warning-clean
/// under `-D warnings`) and used only when the module has a single defined
/// shared memory.
fn shared_memory_runtime_lines() -> Vec<String> {
    const RUNTIME: &str = r#"#[derive(Clone)]
pub struct SharedMemory(std::sync::Arc<SharedMemInner>);

struct SharedMemInner {
    bytes: std::sync::Mutex<Vec<u8>>,
    park: std::sync::Mutex<SharedPark>,
    cvar: std::sync::Condvar,
}

#[derive(Default)]
struct SharedPark {
    // addr -> number of parked waiters
    waiters: std::collections::HashMap<u32, u32>,
    // addr -> notification generation; a waiter wakes when its address's
    // generation advances past the value captured on entry.
    gen: std::collections::HashMap<u32, u64>,
}

fn shared_width_mask(width: usize) -> u64 {
    if width >= 8 { u64::MAX } else { (1u64 << (width * 8)) - 1 }
}

fn shared_read_le(b: &[u8], addr: usize, width: usize) -> u64 {
    let mut v: u64 = 0;
    let mut i = 0;
    while i < width {
        v |= (b[addr + i] as u64) << (8 * i);
        i += 1;
    }
    v
}

fn shared_write_le(b: &mut [u8], addr: usize, width: usize, val: u64) {
    let mut i = 0;
    while i < width {
        b[addr + i] = (val >> (8 * i)) as u8;
        i += 1;
    }
}

// RMW op codes: 0=add 1=sub 2=and 3=or 4=xor 5=xchg
fn shared_combine(op: u8, old: u64, val: u64, width: usize) -> u64 {
    let m = shared_width_mask(width);
    let r = match op {
        0 => old.wrapping_add(val),
        1 => old.wrapping_sub(val),
        2 => old & val,
        3 => old | val,
        4 => old ^ val,
        _ => val, // xchg
    };
    r & m
}

#[allow(dead_code)]
impl SharedMemory {
    fn new(min_bytes: usize) -> Self {
        SharedMemory(std::sync::Arc::new(SharedMemInner {
            bytes: std::sync::Mutex::new(vec![0u8; min_bytes]),
            park: std::sync::Mutex::new(SharedPark::default()),
            cvar: std::sync::Condvar::new(),
        }))
    }

    fn bytes(&self) -> std::sync::MutexGuard<'_, Vec<u8>> {
        self.0.bytes.lock().unwrap()
    }

    // One critical section, so the whole read-modify-write is atomic across
    // threads. Returns the (zero-extended) old value.
    fn atomic_rmw(&self, addr: usize, width: usize, op: u8, val: u64) -> u64 {
        let mut b = self.0.bytes.lock().unwrap();
        let old = shared_read_le(&b, addr, width);
        let new = shared_combine(op, old, val & shared_width_mask(width), width);
        shared_write_le(&mut b, addr, width, new);
        old
    }

    // Stores `replacement` only if the (masked) current value equals `expected`.
    // Returns the (zero-extended) old value either way.
    fn atomic_cmpxchg(&self, addr: usize, width: usize, expected: u64, replacement: u64) -> u64 {
        let mut b = self.0.bytes.lock().unwrap();
        let old = shared_read_le(&b, addr, width);
        if old == (expected & shared_width_mask(width)) {
            shared_write_le(&mut b, addr, width, replacement & shared_width_mask(width));
        }
        old
    }

    fn notify(&self, addr: u32, count: u32) -> u32 {
        let mut park = self.0.park.lock().unwrap();
        let waiting = park.waiters.get(&addr).copied().unwrap_or(0);
        let n = waiting.min(count);
        if n > 0 {
            let g = park.gen.get(&addr).copied().unwrap_or(0).wrapping_add(1);
            park.gen.insert(addr, g);
            drop(park);
            self.0.cvar.notify_all();
        }
        n
    }

    // Returns 0 (woken), 1 (value mismatch - did not block), or 2 (timed out).
    // `timeout_ns < 0` means wait forever. `width` is 4 (wait32) or 8 (wait64).
    fn wait(&self, addr: u32, expected: u64, width: usize, timeout_ns: i64) -> i32 {
        // Hold `park` across the value check so a concurrent `notify` (which also
        // takes `park`) cannot slip between the check and our registration -
        // closing the lost-wakeup window. Lock order is always park-then-bytes.
        let mut park = self.0.park.lock().unwrap();
        {
            let b = self.0.bytes.lock().unwrap();
            if shared_read_le(&b, addr as usize, width) != (expected & shared_width_mask(width)) {
                return 1;
            }
        }
        let start = park.gen.get(&addr).copied().unwrap_or(0);
        *park.waiters.entry(addr).or_insert(0) += 1;
        let result;
        if timeout_ns < 0 {
            loop {
                if park.gen.get(&addr).copied().unwrap_or(0) != start {
                    result = 0;
                    break;
                }
                park = self.0.cvar.wait(park).unwrap();
            }
        } else {
            let deadline = std::time::Instant::now()
                + std::time::Duration::from_nanos(timeout_ns as u64);
            loop {
                if park.gen.get(&addr).copied().unwrap_or(0) != start {
                    result = 0;
                    break;
                }
                let now = std::time::Instant::now();
                if now >= deadline {
                    result = 2;
                    break;
                }
                let (g, to) = self.0.cvar.wait_timeout(park, deadline - now).unwrap();
                park = g;
                if to.timed_out() && park.gen.get(&addr).copied().unwrap_or(0) == start {
                    result = 2;
                    break;
                }
            }
        }
        if let Some(c) = park.waiters.get_mut(&addr) {
            *c = c.saturating_sub(1);
        }
        result
    }
}"#;
    RUNTIME.lines().map(|l| l.to_string()).collect()
}

/// Whether the module needs the injected `Imports` host trait — i.e. it has a
/// non-WASI function import, an imported global, or host-owned memory/table.
/// Recognised WASI imports are native inherent methods and need no host, so a
/// module whose imports are all WASI is fully standalone.
fn needs_host_trait(parts: &ModuleParts<'_>) -> bool {
    parts.imports.iter().any(|im| im.wasi.is_none())
        || !parts.imported_globals.is_empty()
        || parts.memories.iter().any(|m| m.imported)
        || parts.table.is_some_and(|t| t.imported)
}

/// The generic-parameter fragments for the `Instance` type: `<H: Imports>` where
/// the parameter is bound (struct/impl headers) and `<H>` where the type is
/// merely named (`Instance<H>`), or empty strings when no host is needed.
fn host_generics(parts: &ModuleParts<'_>) -> (&'static str, &'static str) {
    if needs_host_trait(parts) {
        ("<H: Imports>", "<H>")
    } else {
        ("", "")
    }
}

/// Render the *prelude* of one chunk file of a multi-file module: the
/// `use super::*;` that pulls the root's items (the `Instance` type, the
/// `Imports` trait, the runtime helpers and the other chunks' re-exported
/// functions) into scope, and — for a stateful module — the opening of the
/// `impl Instance` block the chunk's methods live in.
///
/// The functions themselves are streamed in after this prelude by
/// [`generate_function_into`](super::generate_function_into): a stateless
/// module's are free `pub fn`s emitted directly, while a stateful module's are
/// `&mut self` methods indented one level inside the `impl` (Rust allows a
/// type's inherent impl to be split across files in the same crate), whose
/// closing `}` the caller appends. The private helper methods the bodies call
/// live on the root's impl and are visible here because a chunk module is a
/// descendant of the crate root that defines them.
pub(super) fn chunk_prelude(parts: &ModuleParts<'_>, stateful: bool) -> String {
    // A chunk that happens to call nothing from the root leaves the glob unused,
    // which `-D warnings` rejects, so the import is unconditionally allowed.
    let mut out = String::from("#[allow(unused_imports)]\nuse super::*;\n");
    if stateful {
        let (decl_generics, type_generics) = host_generics(parts);
        out.push('\n');
        out.push_str(ALLOW);
        out.push('\n');
        out.push_str(&format!("impl{decl_generics} Instance{type_generics} {{\n"));
    }
    out
}

/// Render the `lib.rs` root of a multi-file module: the module-scope runtime
/// helpers followed by `mod`/`pub use` declarations for each chunk.
///
/// A stateless root re-exports every chunk's free functions so callers (and the
/// other chunks' `use super::*`) see them at the crate root, matching the
/// single-file layout. A stateful root additionally emits the `Instance` struct,
/// its `new`, the shared helper/dispatch methods and the `Imports` trait — every
/// part of [`render_module`] except the per-function bodies, which live in the
/// chunk files.
/// The module-wide helper dependencies aggregated across every function, needed
/// only to render the crate root. Grouped so the root renderer takes one set
/// reference instead of four separate arguments.
pub(super) struct RootDeps<'a> {
    pub(super) helpers: &'a HashSet<(Helper, u32)>,
    pub(super) rt: &'a HashSet<super::Rt>,
    pub(super) simd: &'a HashSet<&'static str>,
    pub(super) dispatch_sigs: &'a HashSet<u32>,
}

pub(super) fn render_lib_root(
    parts: &ModuleParts<'_>,
    ctx: &ModuleCtx<'_>,
    stateful: bool,
    deps: &RootDeps<'_>,
    n_chunks: usize,
) -> Result<String, TranspileError> {
    let mut out = String::new();
    let rt_helpers = render_rt_helpers(deps.rt);
    if !rt_helpers.is_empty() {
        out.push_str(&rt_helpers);
        out.push('\n');
    }
    let simd_helpers = super::render_simd_helpers(deps.simd);
    if !simd_helpers.is_empty() {
        out.push_str(&simd_helpers);
        out.push('\n');
    }

    if stateful {
        // The header impl carries no function bodies; those are added by the
        // chunk files' own `impl Instance` blocks.
        out.push_str(&render_module(
            parts,
            ctx,
            &[],
            deps.helpers,
            deps.dispatch_sigs,
        )?);
        out.push('\n');
    }

    for i in 0..n_chunks {
        out.push_str(&format!("mod funcs_{i};\n"));
    }
    // A stateless module exposes free functions at the crate root, so the chunks
    // must be re-exported to preserve that public shape.
    if !stateful {
        for i in 0..n_chunks {
            out.push_str(&format!("pub use funcs_{i}::*;\n"));
        }
    }
    Ok(out)
}

/// The `pub fn call_ref_t{ti}(&mut self, f: u32, args..) -> res` dispatch method
/// for `call_indirect` type index `ti`: a `match` on the funcref that routes
/// each function whose signature matches (spanning imports then defined
/// functions) to `call_expr`, with a catch-all trap for null or a wrong type.
/// Made `pub` so the host can invoke a funcref the module exported (the outbound
/// half of cross-instance dispatch).
fn dispatch_method_lines(
    ctx: &ModuleCtx<'_>,
    type_index: u32,
    has_imports: bool,
) -> Result<Vec<String>, TranspileError> {
    let sig = ctx
        .types
        .get(type_index as usize)
        .ok_or_else(|| TranspileError::Unsupported("call_indirect: unknown type".into()))?;
    // Structural type equality (no subtyping), matching `call_indirect`.
    let want = Some((sig.params.as_slice(), sig.results.as_slice()));
    let targets = (0..ctx.func_count()).filter(|&fidx| ctx.full_sig(fidx) == want);

    let mut params = String::from("&mut self, f: u32");
    for (k, ty) in sig.params.iter().enumerate() {
        params.push_str(&format!(", a{k}: {}", rust_type(*ty, ctx.type_kinds)?));
    }
    let ret = match sig.results.as_slice() {
        [] => String::new(),
        [ty] => format!(" -> {}", rust_type(*ty, ctx.type_kinds)?),
        many => format!(" -> ({})", rust_types(many, ctx.type_kinds)?.join(", ")),
    };
    let arg_list = (0..sig.params.len())
        .map(|k| format!("a{k}"))
        .collect::<Vec<_>>()
        .join(", ");
    let sep = if arg_list.is_empty() { "" } else { ", " };

    let mut lines = vec![
        format!("pub fn call_ref_t{type_index}({params}){ret} {{"),
        "    match f {".to_string(),
    ];
    for fidx in targets {
        let expr = ctx.call_expr(fidx, &arg_list);
        lines.push(format!("        {fidx}u32 => {expr},"));
    }
    // The high bit tags an external handle the host resolves (see the trait's
    // defaulted `call_ref_t{ti}`); the tag is stripped before the host sees the
    // slot. `u32::MAX` (null) has the high bit set too, so it is matched first
    // and traps rather than being forwarded. Only a module with a host (an
    // `Imports` trait) can receive external handles.
    if has_imports {
        lines.push("        u32::MAX => panic!(\"indirect call type mismatch\"),".to_string());
        lines.push(format!(
            "        h if (h & 0x8000_0000u32) != 0 => \
             self.imports.call_ref_t{type_index}(h & 0x7fff_ffffu32{sep}{arg_list}),"
        ));
    }
    lines.push("        _ => panic!(\"indirect call type mismatch\"),".to_string());
    lines.push("    }".to_string());
    lines.push("}".to_string());
    Ok(lines)
}

/// A module-scope `ExternFuncs{ti}` registry per `call_indirect` signature: a
/// small helper the host uses to link external funcrefs without hand-managing
/// slot numbers or the handle tag bit. `register` stores a boxed closure and
/// returns a tagged funcref handle to place in a table; `call` resolves a
/// stripped slot (as delivered to `Imports::call_ref_t{ti}`) back to its
/// closure. Emitted only for modules that can receive external handles, and
/// `#[allow(dead_code)]` since a host may ignore it.
fn extern_registry_lines(
    ctx: &ModuleCtx<'_>,
    dispatch_sigs: &HashSet<u32>,
) -> Result<Vec<String>, TranspileError> {
    let mut ordered: Vec<u32> = dispatch_sigs.iter().copied().collect();
    ordered.sort_unstable();

    let mut lines = Vec::new();
    for ti in ordered {
        let sig = ctx
            .types
            .get(ti as usize)
            .ok_or_else(|| TranspileError::Unsupported("call_indirect: unknown type".into()))?;
        let param_tys = rust_types(&sig.params, ctx.type_kinds)?.join(", ");
        let ret = match sig.results.as_slice() {
            [] => String::new(),
            [ty] => format!(" -> {}", rust_type(*ty, ctx.type_kinds)?),
            many => format!(" -> ({})", rust_types(many, ctx.type_kinds)?.join(", ")),
        };
        let boxed = format!("Box<dyn FnMut({param_tys}){ret}>");
        let mut call_params = String::from("&mut self, slot: u32");
        for (k, ty) in sig.params.iter().enumerate() {
            call_params.push_str(&format!(", a{k}: {}", rust_type(*ty, ctx.type_kinds)?));
        }
        let arg_list = (0..sig.params.len())
            .map(|k| format!("a{k}"))
            .collect::<Vec<_>>()
            .join(", ");

        lines.push(String::new());
        lines.push("#[allow(dead_code)]".to_string());
        lines.push(format!("pub struct ExternFuncs{ti} {{"));
        lines.push(format!("    fns: Vec<{boxed}>,"));
        lines.push("}".to_string());
        lines.push("#[allow(dead_code)]".to_string());
        lines.push(format!("impl ExternFuncs{ti} {{"));
        lines.push("    pub fn new() -> Self {".to_string());
        lines.push("        Self { fns: Vec::new() }".to_string());
        lines.push("    }".to_string());
        lines.push(format!(
            "    pub fn register(&mut self, f: {boxed}) -> u32 {{"
        ));
        lines.push("        let slot = self.fns.len() as u32;".to_string());
        lines.push("        self.fns.push(f);".to_string());
        lines.push("        0x8000_0000u32 | slot".to_string());
        lines.push("    }".to_string());
        lines.push(format!("    pub fn call({call_params}){ret} {{"));
        lines.push(format!("        (self.fns[slot as usize])({arg_list})"));
        lines.push("    }".to_string());
        lines.push("}".to_string());
    }
    Ok(lines)
}

/// The `pub trait Imports` declaration: one `import{j}` method per imported
/// function, taking `&mut self` since a host call may have side effects.
fn import_trait_lines(
    ctx: &ModuleCtx<'_>,
    imports: &[ImportInfo],
    imported_globals: &[ImportedGlobalInfo],
    mem_imported: bool,
    table_imported: bool,
    dispatch_sigs: &HashSet<u32>,
) -> Result<Vec<String>, TranspileError> {
    let mut lines = vec!["pub trait Imports {".to_string()];
    // Imported memory: the host lends its buffer for loads/stores/grow.
    if mem_imported {
        lines.push("    fn memory(&self) -> &[u8];".to_string());
        lines.push("    fn memory_mut(&mut self) -> &mut Vec<u8>;".to_string());
    }
    // Imported table: the host lends its `Vec<u32>` for get/set/size/grow and
    // for indirect-call dispatch (each entry is a function index).
    if table_imported {
        lines.push("    fn table(&self) -> &[u32];".to_string());
        lines.push("    fn table_mut(&mut self) -> &mut Vec<u32>;".to_string());
    }
    for (j, im) in imports.iter().enumerate() {
        // A recognised WASI import is a native inherent method, not a host
        // trait method; skip it while keeping `import{j}` aligned to the
        // absolute import index used by `call_expr`.
        if im.wasi.is_some() {
            continue;
        }
        let mut params = String::from("&mut self");
        for (k, ty) in im.params.iter().enumerate() {
            params.push_str(&format!(", a{k}: {}", rust_type(*ty, ctx.type_kinds)?));
        }
        let ret = match im.results.as_slice() {
            [] => String::new(),
            [ty] => format!(" -> {}", rust_type(*ty, ctx.type_kinds)?),
            _ => {
                return Err(TranspileError::Unsupported(
                    "multi-value import result".into(),
                ));
            }
        };
        lines.push(format!("    fn import{j}({params}){ret};"));
    }
    // Each imported global gets a getter; a mutable one also gets a setter.
    for (k, g) in imported_globals.iter().enumerate() {
        let ty = rust_type(g.ty, ctx.type_kinds)?;
        lines.push(format!("    fn get_global{k}(&self) -> {ty};"));
        if g.mutable {
            lines.push(format!("    fn set_global{k}(&mut self, v: {ty});"));
        }
    }
    // One `call_ref_t{ti}` per `call_indirect` signature, resolving an external
    // funcref handle (a high-bit-tagged table entry) to another instance's
    // function. It defaults to a trap so hosts that never place an external
    // handle need not implement it; a host wanting cross-instance dispatch
    // overrides it. The `slot` is the handle with its tag bit already stripped.
    let mut dispatch_ordered: Vec<u32> = dispatch_sigs.iter().copied().collect();
    dispatch_ordered.sort_unstable();
    for ti in dispatch_ordered {
        let sig = ctx
            .types
            .get(ti as usize)
            .ok_or_else(|| TranspileError::Unsupported("call_indirect: unknown type".into()))?;
        let ret = match sig.results.as_slice() {
            [] => String::new(),
            [ty] => format!(" -> {}", rust_type(*ty, ctx.type_kinds)?),
            many => format!(" -> ({})", rust_types(many, ctx.type_kinds)?.join(", ")),
        };
        // Underscore the parameters: the default body ignores them, and an
        // override is free to rename them.
        let mut params = String::from("&mut self, _slot: u32");
        for (k, ty) in sig.params.iter().enumerate() {
            params.push_str(&format!(", _a{k}: {}", rust_type(*ty, ctx.type_kinds)?));
        }
        lines.push(format!(
            "    fn call_ref_t{ti}({params}){ret} \
             {{ panic!(\"unresolved external funcref\") }}"
        ));
    }
    lines.push("}".to_string());
    Ok(lines)
}
