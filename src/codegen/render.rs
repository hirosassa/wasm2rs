use std::collections::HashSet;

use super::helpers::{HELPER_ORDER, helper_lines, shared_helper_lines, table_accessor};
use super::render_cont::{continuation_method_lines, continuation_runtime_lines};
use super::runtime::render_rt_helpers;
use super::wasi::{WasiAccess, confine_lines};
use super::{
    ALLOW, DataSegment, ElemSegment, ExportInfo, GlobalExportInfo, Helper, ImportInfo,
    ImportedGlobalInfo, ModuleCtx, ModuleParts, SegOffset, TableFill, WASM_PAGE_SIZE, WasiFn,
    data_static_line, indent, index_u32, is_gc_table_element, rust_type, rust_types,
    table_elem_rust, table_null,
};
use crate::TranspileError;
use wasmparser::ValType;

/// High bit tagging a funcref handle as an *external* (host-provided) callback
/// rather than one of the module's own function indices. Emitted verbatim into
/// generated code, so it is the underscored literal token, not a `u32`.
const EXTERNAL_HANDLE_BIT: &str = "0x8000_0000u32";
/// Complement of [`EXTERNAL_HANDLE_BIT`]: masks a tagged handle back to the raw
/// external slot before the host resolves it.
const EXTERNAL_HANDLE_SLOT_MASK: &str = "0x7fff_ffffu32";

/// The layout flags derived once and shared across every `Instance` render
/// section: whether the module needs an injected host trait, whether memory 0 /
/// any table are host-owned (so the instance stores no field for them), whether
/// any file-backed WASI import is present (so the `wasi_fds` table exists), and
/// the global-index base past the imported globals.
struct Layout {
    has_imports: bool,
    mem_imported: bool,
    table_imported: bool,
    wasi_files: bool,
    wasi_preopen: bool,
    wasi_threads: bool,
    global_base: usize,
}

impl Layout {
    fn of(parts: &ModuleParts<'_>) -> Self {
        // Only memory 0 may be imported (an imported non-zero memory is rejected
        // during parsing); `mem_imported` therefore governs just memory 0.
        // An imported *shared* memory is a handle passed to `new`, not lent by the host.
        let mem_imported = parts
            .memories
            .first()
            .is_some_and(|m| m.imported && !m.shared);
        let table_imported = parts
            .table
            .into_iter()
            .chain(parts.extra_tables)
            .any(|t| t.imported);
        // A module that imports any preopen/`path_open` function gains a real
        // file-descriptor table (`wasi_fds`) and the file-backed fd_* variants.
        let wasi_files = parts.imports.iter().any(|im| {
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
        // Any path-resolving/preopen-naming WASI import (broader than
        // `wasi_files`, which needs only the fd-table subset) requires the
        // configurable preopen root, defaulting to the process CWD (".").
        let wasi_preopen = parts
            .imports
            .iter()
            .any(|im| im.wasi.is_some_and(WasiFn::resolves_path));
        let wasi_threads = parts
            .imports
            .iter()
            .any(|im| im.wasi.is_some_and(WasiFn::spawns_threads));
        Layout {
            has_imports: needs_host_trait(parts),
            mem_imported,
            table_imported,
            wasi_files,
            wasi_preopen,
            wasi_threads,
            // Defined globals are named by their full index, i.e. after the
            // imported globals in the shared global index space.
            global_base: parts.imported_globals.len(),
        }
    }
}

/// Render the `struct Instance` and its `impl` for a stateful module. When the
/// module imports functions, a `pub trait Imports` is emitted and `Instance`
/// becomes generic over a host `H: Imports` that it stores and dispatches to.
///
/// The body is assembled by combining the per-section renderers below: the
/// module-scope declarations (host trait, shared-memory runtime, passive
/// statics, continuation runtime) come first, then the `struct` fields, then the
/// `impl`'s inherent methods (constructors, memory/table accessors, WASI/helper/
/// dispatch/continuation methods), and finally each function's own source
/// streamed in.
fn render_module(
    out: &mut String,
    parts: &ModuleParts<'_>,
    ctx: &ModuleCtx<'_>,
    sources: &[&str],
    used: &HashSet<(Helper, u32)>,
    dispatch_sigs: &HashSet<u32>,
) -> Result<(), TranspileError> {
    render_module_head(out, parts, ctx, used, dispatch_sigs)?;
    // Stream each function's source straight into `out`, indenting as it is
    // written: no per-line `String`, no intermediate line vector and no final
    // `join` copy of the whole module.
    for src in sources {
        push_function_source(out, src);
    }
    render_module_tail(out, parts, ctx, dispatch_sigs)
}

/// Everything in a stateful module that precedes the method bodies: the
/// `Imports` trait, the segment statics, the `Instance` struct and the
/// `impl Instance` block up to (not including) the functions.
pub(super) fn render_module_head(
    out: &mut String,
    parts: &ModuleParts<'_>,
    ctx: &ModuleCtx<'_>,
    used: &HashSet<(Helper, u32)>,
    dispatch_sigs: &HashSet<u32>,
) -> Result<(), TranspileError> {
    let layout = Layout::of(parts);

    let mut lines: Vec<String> = Vec::new();

    // Under the opt-in `unsafe_memory` mode the emitted linear-memory access is
    // unchecked, so an out-of-bounds access is undefined behaviour rather than a
    // wasm trap. Flag that at the top of the module (off by default, so the safe
    // output stays byte-for-byte unchanged).
    if ctx.unsafe_memory {
        lines.push(
            "// Built with `unsafe_memory`: linear-memory loads/stores and bulk `memory.*`"
                .to_string(),
        );
        lines.push(
            "// are UNCHECKED. An out-of-bounds access is undefined behaviour, not a wasm"
                .to_string(),
        );
        lines.push("// trap. Only sound for modules trusted to stay in bounds.".to_string());
        lines.push(String::new());
    }

    // The `pub trait Imports` the instance dispatches host calls through.
    if layout.has_imports {
        lines.extend(import_trait_lines(
            ctx,
            parts.imports,
            parts.imported_globals,
            layout.mem_imported,
            parts
                .table
                .into_iter()
                .chain(parts.extra_tables)
                .filter(|t| t.imported)
                .count(),
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
    // Module-scope statics backing the data and element segments.
    lines.extend(segment_statics(parts.data, parts.elements));
    // Typed-continuations runtime: the step result, the per-continuation frame
    // structs and the tagged object stored in the instance's `conts` table.
    if !ctx.cont.bodies.is_empty() {
        lines.extend(continuation_runtime_lines(ctx)?);
        lines.push(String::new());
    }

    // The `Instance` struct itself, then the opening of its inherent `impl`.
    lines.extend(instance_field_lines(parts, ctx, &layout)?);
    lines.push(String::new());

    let (decl_generics, type_generics) = host_generics(parts);
    lines.push(ALLOW.to_string());
    lines.push(format!("impl{decl_generics} Instance{type_generics} {{"));

    // Everything inside the `impl` is collected unindented, then indented once.
    // Each section renderer reproduces the blank-line separators it needs, so
    // this is a straight concatenation.
    let mut inner: Vec<String> = Vec::new();
    inner.extend(constructor_lines(parts, ctx, &layout)?);
    if ctx.reentrant_imports {
        inner.extend(with_host_lines());
    }
    inner.extend(memory_accessor_lines(parts, ctx));
    inner.extend(table_accessor_lines(parts)?);
    inner.extend(wasi_method_lines(parts.imports, &layout, ctx.memory_shared));
    inner.extend(helper_method_lines(
        used,
        parts.memories,
        ctx.memory_shared,
        ctx.unsafe_memory,
    ));
    inner.extend(dispatch_method_block(
        ctx,
        dispatch_sigs,
        layout.has_imports,
    )?);
    inner.extend(entry_point_lines(parts, ctx)?);
    // The continuation allocator (`cont.new`) and stepper (`resume`).
    if !ctx.cont.bodies.is_empty() {
        inner.push(String::new());
        inner.extend(continuation_method_lines(ctx));
    }

    for line in indent(&inner) {
        lines.push(line);
    }
    push_lines(out, &lines);
    Ok(())
}

/// What follows the method bodies of a stateful module: the closing brace of
/// the `impl Instance` block and the external-funcref registries.
pub(super) fn render_module_tail(
    out: &mut String,
    parts: &ModuleParts<'_>,
    ctx: &ModuleCtx<'_>,
    dispatch_sigs: &HashSet<u32>,
) -> Result<(), TranspileError> {
    let layout = Layout::of(parts);
    out.push_str("}\n");

    // Module-scope external-funcref registries (the Linker helper), one per
    // `call_indirect` signature, for modules that can receive external handles.
    if layout.has_imports && !dispatch_sigs.is_empty() {
        push_lines(out, &extern_registry_lines(ctx, dispatch_sigs)?);
    }
    Ok(())
}

/// Append `lines` to `out`, each newline-terminated.
fn push_lines(out: &mut String, lines: &[String]) {
    for line in lines {
        out.push_str(line);
        out.push('\n');
    }
}

/// Append one function's source to `out` as a blank-line-separated block
/// indented by four spaces (blank lines stay empty).
fn push_function_source(out: &mut String, src: &str) {
    out.push('\n');
    for line in src.lines() {
        if !line.is_empty() {
            out.push_str("    ");
            out.push_str(line);
        }
        out.push('\n');
    }
}

/// Module-scope `static`s backing the data and element segments. Every data
/// segment's bytes live in `DATA{d}`: an active one is copied from it when the
/// instance is built, a passive one is kept as a `&'static` slice of it so
/// `memory.init` can copy from it on demand. Passive element segments likewise
/// back `table.init`.
fn segment_statics(data: &[DataSegment], elements: &[ElemSegment]) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for (d, seg) in data.iter().enumerate() {
        lines.push(data_static_line(d, &seg.bytes));
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
    lines
}

/// The `pub struct Instance { … }` declaration: the host handle (when needed),
/// each defined memory buffer (or the shared handle), the table, the globals,
/// the WASI fd table, the retained passive-segment slices, the externref box and
/// the continuation handle table — whichever the module actually needs.
fn instance_field_lines(
    parts: &ModuleParts<'_>,
    ctx: &ModuleCtx<'_>,
    layout: &Layout,
) -> Result<Vec<String>, TranspileError> {
    let ModuleParts {
        globals,
        memories,
        data,
        elements,
        ..
    } = *parts;
    let (decl_generics, _) = host_generics(parts);

    let mut lines: Vec<String> = vec![
        "#[allow(dead_code)]".to_string(),
        format!("pub struct Instance{decl_generics} {{"),
    ];
    if layout.has_imports {
        // A re-entrant host is lent out for the duration of each import call.
        let ty = if ctx.reentrant_imports {
            "Option<Box<H>>"
        } else {
            "H"
        };
        lines.push(format!("    imports: {ty},"));
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
            if i == 0 && layout.mem_imported {
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
    // Imported tables live in the host, so the instance owns no storage for them.
    for (i, t) in parts
        .table
        .into_iter()
        .chain(parts.extra_tables)
        .enumerate()
    {
        if t.imported {
            continue;
        }
        // A table entry is a function index; `u32::MAX` marks a null funcref. An
        // `exnref` table holds managed `GcRef`s instead.
        let (field, _) = table_accessor(index_u32(i)?);
        lines.push(format!("    {field}: Vec<{}>,", table_elem_rust(t.element)));
    }
    for (i, g) in globals.iter().enumerate() {
        lines.push(format!(
            "    g{}: {},",
            layout.global_base + i,
            rust_type(g.ty, ctx.type_kinds)?
        ));
    }
    // Files/directories opened through `path_open`; descriptor N is
    // `wasi_fds[N - 4]` (0-2 are stdio, 3 is the preopen directory). Each slot
    // keeps the open handle and its containment-checked relative path (the path
    // is what `fd_readdir` re-opens with `read_dir`, since a `File` cannot be
    // enumerated directly with std alone).
    if layout.wasi_files {
        // Threads of one process share the table (descriptor numbers are
        // process-wide), so it sits behind an `Arc<Mutex<..>>` when the module
        // can spawn them.
        let table = "Vec<Option<(std::fs::File, std::path::PathBuf)>>";
        lines.push(if layout.wasi_threads {
            format!("    wasi_fds: std::sync::Arc<std::sync::Mutex<{table}>>,")
        } else {
            format!("    wasi_fds: {table},")
        });
    }
    // The single WASI preopen's host root: guest paths resolve within it and its
    // name is advertised through `fd_prestat_dir_name`. Defaults to the process
    // CWD ("."); `with_preopen_root` re-points it.
    if layout.wasi_preopen {
        lines.push("    wasi_preopen: std::path::PathBuf,".to_string());
    }
    // The next thread id handed out by `wasi::thread-spawn`, shared by every
    // thread of the process (each child instance holds a clone of the `Arc`).
    if layout.wasi_threads {
        lines.push("    wasi_tid: std::sync::Arc<std::sync::atomic::AtomicI32>,".to_string());
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
    // The continuation handle table: a `cont.new` stores a fresh resumable
    // object in a free slot and returns a generation-tagged handle to it; a slot
    // whose continuation has run to completion (one-shot, consumed on return) is
    // recycled.
    if !ctx.cont.bodies.is_empty() {
        lines.push("    conts: Vec<ContSlot>,".to_string());
        // Indices of slots whose continuation has returned, ready for reuse.
        lines.push("    cont_free: Vec<u32>,".to_string());
    }
    lines.push("}".to_string());
    Ok(lines)
}

/// Open a block that reads a runtime segment offset once, as `off`. `host` names
/// the host the offset reads (`imports`, the constructor's parameter, or
/// `instance.imports` once it has been moved into the built instance), bound to
/// `imports` for the expression. An offset that does not fit in memory saturates
/// to `usize::MAX`, which is out of bounds in the target. The caller closes the
/// block.
fn open_runtime_offset(lines: &mut Vec<String>, expr: &str, host: &str, indent: &str) {
    lines.push(format!("{indent}{{"));
    lines.push(format!(
        "{indent}    let off = usize::try_from({{ let imports = &{host}; {expr} }}).unwrap_or(usize::MAX);"
    ));
}

/// A table holds at most `u32::MAX` entries, so any slot at or above that is out
/// of range: clamping keeps the spelled literal valid on a 32-bit `usize` while
/// the access still traps.
fn elem_slot(slot: u64) -> u64 {
    slot.min(u64::from(u32::MAX))
}

/// The byte size of `pages` wasm pages.
fn page_bytes(pages: u64) -> Result<u64, TranspileError> {
    pages
        .checked_mul(WASM_PAGE_SIZE)
        .ok_or_else(|| TranspileError::Unsupported("memory too large".into()))
}

/// The opening and closing lines of the `Self { .. }` literal: bound to a mutable
/// `instance` when post-construction steps must run on it, returned directly
/// otherwise.
fn struct_literal_delims(bind: bool) -> (&'static str, &'static str) {
    if bind {
        ("    let mut instance = Self {", "    };")
    } else {
        ("    Self {", "    }")
    }
}

/// What the constructors need to know about the module's active segments and
/// post-construction steps, derived once and shared by the `*_init_lines` helpers.
struct CtorPlan<'a> {
    parts: &'a ModuleParts<'a>,
    ctx: &'a ModuleCtx<'a>,
    layout: &'a Layout,
    /// Only active segments are copied at instantiation; passive ones are
    /// retained (see the `data{d}` fields) for `memory.init`. Each active data
    /// segment names the memory it initialises.
    active: Vec<(usize, &'a DataSegment)>,
    active_elem: Vec<&'a ElemSegment>,
    /// Imported memory/table are host-owned, so their active data/elements cannot
    /// be written into a `memory`/`table` field literal; instead the instance is
    /// bound and the segments are copied into the host storage through
    /// `mem_mut()`/`table{i}_mut()` after construction. With any imported table, every
    /// active element segment is applied that way in module order (a trap then leaves
    /// the earlier segments' writes in the host tables, as the spec says). Only
    /// memory 0 can be imported, so only its segments need post-construction copying, unless a
    /// segment also targets a defined memory: then every data segment is applied
    /// after construction, in module order (a trap leaves the earlier segments'
    /// writes in the host memory, and the elements are already in place), as for
    /// tables.
    data_in_module_order: bool,
    post_init_data: bool,
    post_init_elem: bool,
    /// The start function runs as the last step of every instantiation, on the
    /// fully built instance.
    start_call: Option<String>,
}

impl<'a> CtorPlan<'a> {
    fn new(parts: &'a ModuleParts<'a>, ctx: &'a ModuleCtx<'a>, layout: &'a Layout) -> Self {
        let active: Vec<(usize, &DataSegment)> = parts
            .data
            .iter()
            .enumerate()
            .filter(|(_, d)| d.offset.is_some())
            .collect();
        let active_elem: Vec<&ElemSegment> = parts
            .elements
            .iter()
            .filter(|e| e.offset.is_some())
            .collect();
        let data_in_module_order = layout.mem_imported
            && !ctx.memory_shared
            && active.iter().any(|(_, d)| d.mem_index > 0);
        let post_init_data = data_in_module_order
            || (layout.mem_imported && active.iter().any(|(_, d)| d.mem_index == 0));
        let post_init_elem = layout.table_imported && !active_elem.is_empty();
        let start_call = parts.start.map(|f| {
            ctx.call_expr(f as usize, "")
                .replacen("self.", "instance.", 1)
        });
        Self {
            parts,
            ctx,
            layout,
            active,
            active_elem,
            data_in_module_order,
            post_init_data,
            post_init_elem,
            start_call,
        }
    }

    /// Whether the constructor must bind the built instance before returning it.
    fn post_init(&self) -> bool {
        self.post_init_data || self.post_init_elem || self.start_call.is_some()
    }

    /// The active data segments that target a given memory index, in order.
    fn active_for(&self, mem: u32) -> Vec<(usize, &'a DataSegment)> {
        self.active
            .iter()
            .copied()
            .filter(|(_, d)| d.mem_index == mem)
            .collect()
    }

    /// Data for a memory the module creates is applied once, to the shared memory,
    /// so only elements repeat per instance (each sibling has its own host table);
    /// data for a host-owned shared memory is applied by `new` after the elements.
    fn data_after_elems(&self) -> bool {
        self.ctx.memory_shared
            && self.parts.memories.first().is_some_and(|m| m.imported)
            && !self.active_for(0).is_empty()
    }

    /// Shared-memory constructors bind the struct first when active elements must
    /// be copied into a host-owned table, data must follow the elements into a
    /// host-owned memory, or a start function must run on it.
    fn shared_bind(&self) -> bool {
        self.start_call.is_some() || self.post_init_elem || self.data_after_elems()
    }

    /// Emit `<target>[off..end].copy_from_slice(&DATA{d});` for the active data
    /// segment `seg`; `target` is a defined buffer (`m`) or a host buffer accessor
    /// (`instance.mem_mut()`), `host` names the host a runtime offset reads, and
    /// `indent` matches the surrounding block.
    fn copy_data_seg(
        lines: &mut Vec<String>,
        d: usize,
        seg: &DataSegment,
        target: &str,
        host: &str,
        indent: &str,
    ) {
        let len = u64::try_from(seg.bytes.len()).unwrap_or(u64::MAX);
        let (base, runtime) = SegOffset::split(seg.offset.as_ref());
        if let Some(expr) = runtime {
            open_runtime_offset(lines, expr, host, indent);
            let copy =
                format!("{target}[off..off.saturating_add({len})].copy_from_slice(&DATA{d});");
            lines.push(format!("{indent}    {copy}"));
            lines.push(format!("{indent}}}"));
            return;
        }
        // The offset is only spelled into the generated source, so it stays a
        // `u64` (a 64-bit memory's segment can lie above 4 GiB); an end that
        // would overflow saturates and is then out of bounds in the target.
        let end = base.saturating_add(len);
        lines.push(format!(
            "{indent}{target}[{base}..{end}].copy_from_slice(&DATA{d});"
        ));
    }

    /// The active data segments targeting memory `mem`, in module order.
    fn copy_active_data(
        &self,
        lines: &mut Vec<String>,
        mem: u32,
        target: &str,
        host: &str,
        indent: &str,
    ) {
        for (d, seg) in self.active_for(mem) {
            Self::copy_data_seg(lines, d, seg, target, host, indent);
        }
    }

    /// Emit `<target>[idx] = fu32;` for each active element segment entry;
    /// `target` is a defined table (`t`) or the host storage accessor
    /// (`instance.table_mut()`). `imports` names the host an imported-global item or
    /// a runtime offset reads: the constructor's parameter, or the built instance's
    /// field once it has been moved in.
    fn copy_elem_seg(
        &self,
        lines: &mut Vec<String>,
        seg: &ElemSegment,
        target: &str,
        imports: &str,
        indent: &str,
    ) {
        // A runtime offset is read once, in a block of its own; a constant
        // one is spelled into each slot.
        let (base, runtime) = SegOffset::split(seg.offset.as_ref());
        let ind = match runtime {
            Some(expr) => {
                open_runtime_offset(lines, expr, imports, indent);
                format!("{indent}    ")
            }
            None => indent.to_string(),
        };
        // An empty segment writes nothing, but an offset past the end of the
        // table still traps at instantiation: check it with an empty slice.
        if seg.funcs.is_empty() {
            let off = if runtime.is_some() {
                "off".to_string()
            } else {
                elem_slot(base).to_string()
            };
            lines.push(format!("{ind}let _ = &{target}[{off}..{off}];"));
        }
        let gc_table = self
            .ctx
            .table_elements
            .get(seg.table as usize)
            .is_some_and(|e| is_gc_table_element(*e));
        for (k, f) in seg.funcs.iter().enumerate() {
            let idx = if runtime.is_some() {
                format!("off.saturating_add({k})")
            } else {
                elem_slot(base.saturating_add(u64::try_from(k).unwrap_or(u64::MAX))).to_string()
            };
            let value = seg
                .imported_items
                .iter()
                .find(|(item, _)| *item == k)
                .map_or_else(
                    // An `exnref` item can only be `ref.null exn`.
                    || {
                        if gc_table {
                            "GcRef::Null".to_string()
                        } else {
                            format!("{f}u32")
                        }
                    },
                    |(_, global)| format!("{imports}.get_global{global}()"),
                );
            lines.push(format!("{ind}{target}[{idx}] = {value};"));
        }
        if runtime.is_some() {
            lines.push(format!("{indent}}}"));
        }
    }

    /// The active segments of one defined table, in module order.
    fn copy_active_elems(
        &self,
        lines: &mut Vec<String>,
        table_idx: u32,
        target: &str,
        imports: &str,
        indent: &str,
    ) {
        for seg in self.active_elem.iter().filter(|seg| seg.table == table_idx) {
            self.copy_elem_seg(lines, seg, target, imports, indent);
        }
    }

    /// The table field initialisers. An imported table is host-owned (see the
    /// post-construction elem copy), so it has no field. Table 0 keeps the `table`
    /// field and table `i > 0` is `table{i}`, numbered by its index in the module's
    /// table index space.
    fn table_init_lines(&self, inner: &mut Vec<String>) -> Result<(), TranspileError> {
        let parts = self.parts;
        for (i, t) in parts
            .table
            .into_iter()
            .chain(parts.extra_tables)
            .enumerate()
        {
            if t.imported {
                continue;
            }
            let i = index_u32(i)?;
            let (field, _) = table_accessor(i);
            let fill = match t.init {
                Some(TableFill::ImportedGlobal(global)) => {
                    format!("imports.get_global{global}()")
                }
                Some(TableFill::Func(f)) => format!("{f}u32"),
                None => table_null(t.element).to_string(),
            };
            if self.post_init_elem || !self.active_elem.iter().any(|seg| seg.table == i) {
                inner.push(format!("        {field}: vec![{fill}; {}],", t.min));
            } else {
                // Start every slot at the table's initializer (null unless it is
                // `ref.func`), then apply each active element segment.
                inner.push(format!("        {field}: {{"));
                inner.push(format!(
                    "            let mut t: Vec<{}> = vec![{fill}; {}];",
                    table_elem_rust(t.element),
                    t.min
                ));
                self.copy_active_elems(inner, i, "t", "imports", "            ");
                inner.push("            t".to_string());
                inner.push("        },".to_string());
            }
        }
        Ok(())
    }

    fn global_init_lines(&self, inner: &mut Vec<String>) {
        for (i, g) in self.parts.globals.iter().enumerate() {
            inner.push(format!(
                "        g{}: {},",
                self.layout.global_base + i,
                g.init
            ));
        }
    }

    fn wasi_init_lines(&self, inner: &mut Vec<String>) {
        let layout = self.layout;
        if layout.wasi_files {
            inner.push(
                if layout.wasi_threads {
                    "        wasi_fds: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),"
                } else {
                    "        wasi_fds: Vec::new(),"
                }
                .to_string(),
            );
        }
        if layout.wasi_preopen {
            inner.push("        wasi_preopen: std::path::PathBuf::from(\".\"),".to_string());
        }
        if layout.wasi_threads {
            inner.push(
                "        wasi_tid: std::sync::Arc::new(std::sync::atomic::AtomicI32::new(0)),"
                    .to_string(),
            );
        }
    }

    /// Passive segments are retained as `&'static` slices of the module-scope
    /// statics, so `memory.init`/`table.init` can copy from them on demand.
    fn passive_init_lines(&self, inner: &mut Vec<String>) {
        for (d, seg) in self.parts.data.iter().enumerate() {
            if seg.offset.is_none() {
                inner.push(format!("        data{d}: &DATA{d},"));
            }
        }
        for (e, seg) in self.parts.elements.iter().enumerate() {
            if seg.offset.is_none() && !seg.declared {
                inner.push(format!("        elem{e}: &ELEM{e},"));
            }
        }
    }

    /// The non-memory field initialisers (table, globals, wasi_fds, the retained
    /// passive segments, and `imports` last), pushed identically by both
    /// `Instance::new` and — for a shared module — `with_memory`. Only the memory
    /// field differs between the two, so factoring the rest keeps them in lock-step.
    fn push_common_fields(&self, inner: &mut Vec<String>) -> Result<(), TranspileError> {
        self.table_init_lines(inner)?;
        self.global_init_lines(inner);
        self.wasi_init_lines(inner);
        self.passive_init_lines(inner);
        if self.ctx.uses_extern_box {
            inner.push("        extern_box: Vec::new(),".to_string());
        }
        if !self.ctx.cont.bodies.is_empty() {
            inner.push("        conts: Vec::new(),".to_string());
            inner.push("        cont_free: Vec::new(),".to_string());
        }
        // Last, so a global initializer above can still read the host.
        if self.layout.has_imports {
            inner.push(if self.ctx.reentrant_imports {
                "        imports: Some(Box::new(imports)),".to_string()
            } else {
                "        imports,".to_string()
            });
        }
        Ok(())
    }

    /// The steps that run on the bound `instance`: copy the active elements into the
    /// host-owned table. Elements come before data, as in the spec.
    fn push_bound_elems(&self, lines: &mut Vec<String>) {
        if self.post_init_elem {
            for seg in &self.active_elem {
                let (_, get_mut) = table_accessor(seg.table);
                let target = format!("instance.{get_mut}()");
                self.copy_elem_seg(lines, seg, &target, "instance.imports", "    ");
            }
        }
    }

    /// Run the start function and return the instance.
    fn push_bound_end(&self, lines: &mut Vec<String>) {
        if let Some(call) = &self.start_call {
            lines.push(format!("    {call};"));
        }
        lines.push("    instance".to_string());
    }

    /// A single defined shared memory. `new` creates a fresh `SharedMemory`,
    /// applies this module's active data segments into it *once*, and inits
    /// globals/tables; `with_memory` joins an existing handle (sibling
    /// instance on another thread), inits its own globals/tables, and does
    /// NOT re-apply data — the memory is already initialised.
    fn shared_memory_init_lines(&self, inner: &mut Vec<String>) -> Result<(), TranspileError> {
        let has_imports = self.layout.has_imports;
        let new_param = if has_imports { "imports: H" } else { "" };
        let data_after_elems = self.data_after_elems();
        let shared_bind = self.shared_bind();
        let (shared_open, shared_close) = struct_literal_delims(shared_bind);
        let mem0 = self
            .parts
            .memories
            .first()
            .ok_or_else(|| TranspileError::Unsupported("shared memory missing".into()))?;
        let bytes = page_bytes(mem0.min_pages)?;

        if mem0.imported {
            // The host created the memory and hands it in; `new` still owns the
            // (one-time) data initialisation, `with_memory` never repeats it.
            let params = if has_imports {
                "memory: SharedMemory, imports: H"
            } else {
                "memory: SharedMemory"
            };
            inner.push(format!("pub fn new({params}) -> Self {{"));
        } else {
            inner.push(format!("pub fn new({new_param}) -> Self {{"));
            // Reserve up to the declared maximum (a shared memory must declare
            // one); without it, the wasm32 limit that `new` reserves.
            let ctor = match mem0.max_pages {
                Some(max) => {
                    let max_bytes = page_bytes(max)?;
                    format!("SharedMemory::with_max({bytes}, {max_bytes})")
                }
                None => format!("SharedMemory::new({bytes})"),
            };
            inner.push(format!("    let memory = {ctor};"));
        }
        // Data for a host-owned memory waits until the elements are in place.
        if !self.active_for(0).is_empty() && !data_after_elems {
            inner.push("    {".to_string());
            inner.push("        let mut __m = memory.bytes();".to_string());
            self.copy_active_data(inner, 0, "__m", "imports", "        ");
            inner.push("    }".to_string());
        }
        inner.push(shared_open.to_string());
        inner.push("        memory,".to_string());
        self.push_common_fields(inner)?;
        inner.push(shared_close.to_string());
        if shared_bind {
            self.push_bound_elems(inner);
            if data_after_elems {
                inner.push("    {".to_string());
                inner.push("        let mut __m = instance.memory.bytes();".to_string());
                self.copy_active_data(inner, 0, "__m", "instance.imports", "        ");
                inner.push("    }".to_string());
            }
            self.push_bound_end(inner);
        }
        inner.push("}".to_string());

        let with_param = if has_imports {
            "mem: SharedMemory, imports: H"
        } else {
            "mem: SharedMemory"
        };
        inner.push(String::new());
        inner.push(format!("pub fn with_memory({with_param}) -> Self {{"));
        inner.push(shared_open.to_string());
        inner.push("        memory: mem,".to_string());
        self.push_common_fields(inner)?;
        inner.push(shared_close.to_string());
        if shared_bind {
            self.push_bound_elems(inner);
            self.push_bound_end(inner);
        }
        inner.push("}".to_string());

        inner.push(String::new());
        inner.push(
            "pub fn shared_memory(&self) -> SharedMemory { self.memory.clone() }".to_string(),
        );
        Ok(())
    }

    /// Each defined memory's field literal: zero-filled, then each active data
    /// segment for that memory copied in. Memory 0 keeps the `memory` field name
    /// and the exact single-memory rendering; memory `i > 0` is `memory{i}`.
    fn memory_init_lines(&self, inner: &mut Vec<String>) -> Result<(), TranspileError> {
        for (i, m) in self.parts.memories.iter().enumerate() {
            if i == 0 && self.layout.mem_imported {
                continue;
            }
            let field = if i == 0 {
                "memory".to_string()
            } else {
                format!("memory{i}")
            };
            let mem_index = index_u32(i)?;
            let bytes = page_bytes(m.min_pages)?;
            if self.data_in_module_order || self.active_for(mem_index).is_empty() {
                inner.push(format!("        {field}: vec![0u8; {bytes}],"));
            } else {
                // Zero the memory, then copy each active data segment into place.
                inner.push(format!("        {field}: {{"));
                inner.push(format!(
                    "            let mut m: Vec<u8> = vec![0u8; {bytes}];"
                ));
                self.copy_active_data(inner, mem_index, "m", "imports", "            ");
                inner.push("            m".to_string());
                inner.push("        },".to_string());
            }
        }
        Ok(())
    }

    /// `Instance::new` for a module without shared memory.
    fn owned_memory_init_lines(&self, inner: &mut Vec<String>) -> Result<(), TranspileError> {
        let new_param = if self.layout.has_imports {
            "imports: H"
        } else {
            ""
        };
        let post_init = self.post_init();
        let (open, close) = struct_literal_delims(post_init);
        inner.push(format!("pub fn new({new_param}) -> Self {{"));
        inner.push(open.to_string());
        self.memory_init_lines(inner)?;
        self.push_common_fields(inner)?;
        inner.push(close.to_string());
        if post_init {
            // Copy the active element segments, then the data segments, into the
            // host-owned storage. An out-of-bounds write panics here, faithfully
            // reproducing a wasm instantiation trap when a segment does not fit.
            self.push_bound_elems(inner);
            if self.data_in_module_order {
                for (d, seg) in &self.active {
                    let target = if seg.mem_index == 0 {
                        "instance.mem_mut()".to_string()
                    } else {
                        format!("instance.mem{}_mut()", seg.mem_index)
                    };
                    Self::copy_data_seg(inner, *d, seg, &target, "instance.imports", "    ");
                }
            } else if self.post_init_data {
                self.copy_active_data(inner, 0, "instance.mem_mut()", "instance.imports", "    ");
            }
            self.push_bound_end(inner);
        }
        inner.push("}".to_string());
        Ok(())
    }
}

/// The `Instance::new` constructor (plus `with_memory`/`shared_memory` for a
/// shared module): materialise each memory buffer (copying its active data
/// segments into place), initialise the table (applying active element
/// segments), the globals, the WASI fd table and the retained passive segments,
/// and — for host-owned memory/table — copy the active segments into the host
/// storage after construction. Returns the unindented `impl`-body lines.
fn constructor_lines(
    parts: &ModuleParts<'_>,
    ctx: &ModuleCtx<'_>,
    layout: &Layout,
) -> Result<Vec<String>, TranspileError> {
    let plan = CtorPlan::new(parts, ctx, layout);
    let mut inner: Vec<String> = Vec::new();
    if ctx.memory_shared {
        plan.shared_memory_init_lines(&mut inner)?;
    } else {
        plan.owned_memory_init_lines(&mut inner)?;
    }
    // Re-point the single WASI preopen away from the process CWD. Guest
    // `path_*`/`fd_readdir` calls then resolve within `root`, which is also the
    // name advertised through `fd_prestat_dir_name`; absolute-path and `..`
    // escapes are still refused. Chained after `new`, e.g.
    // `Instance::new(host).with_preopen_root("/usr/share/zoneinfo")`.
    if layout.wasi_preopen {
        inner.push(String::new());
        inner.push(
            "pub fn with_preopen_root(mut self, root: impl Into<std::path::PathBuf>) -> Self {"
                .to_string(),
        );
        inner.push("    self.wasi_preopen = root.into();".to_string());
        inner.push("    self".to_string());
        inner.push("}".to_string());
    }
    Ok(inner)
}

/// `with_host`: lend the host out of `imports` for one call, handing it the
/// instance, and put it back, also when the call unwinds (a wasm exception thrown
/// through the host, or a trap the embedder catches). A host call reached from
/// inside another one finds no host and traps. The host is boxed so lending it
/// moves a pointer, not the whole `H`.
fn with_host_lines() -> Vec<String> {
    [
        "fn with_host<R>(&mut self, f: impl FnOnce(&mut H, &mut Self) -> R) -> R {",
        "    let mut host = self.imports.take().expect(\"re-entrant import call\");",
        "    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&mut host, self)));",
        "    self.imports = Some(host);",
        "    match result {",
        "        Ok(value) => value,",
        "        Err(payload) => std::panic::resume_unwind(payload),",
        "    }",
        "}",
    ]
    .map(str::to_string)
    .to_vec()
}

/// The uniform memory accessors (`mem`/`mem_mut`/`memory` for memory 0, then
/// `mem{i}`/`mem{i}_mut`/`memory{i}`), so the load/store/bulk helpers read the
/// same regardless of whether a memory is defined (a field) or imported (lent by
/// the host). A shared module emits none of these — its helpers use
/// `self.memory.bytes()` directly — so the accessor set is empty there.
fn memory_accessor_lines(parts: &ModuleParts<'_>, ctx: &ModuleCtx<'_>) -> Vec<String> {
    let mut inner: Vec<String> = Vec::new();
    for (i, m) in parts
        .memories
        .iter()
        .enumerate()
        .filter(|_| !ctx.memory_shared)
    {
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
    inner
}

/// The uniform table accessors (`table`/`table_mut`), mirroring the memory ones:
/// a defined table is a field, an imported one is lent by the host through the
/// `Imports` trait. Empty when the module declares no table.
fn table_accessor_lines(parts: &ModuleParts<'_>) -> Result<Vec<String>, TranspileError> {
    let mut inner: Vec<String> = Vec::new();
    for (i, t) in parts
        .table
        .into_iter()
        .chain(parts.extra_tables)
        .enumerate()
    {
        let (get, get_mut) = table_accessor(index_u32(i)?);
        let (borrow, borrow_mut) = if t.imported {
            (
                format!("self.imports.{get}()"),
                format!("self.imports.{get_mut}()"),
            )
        } else {
            (format!("&self.{get}"), format!("&mut self.{get}"))
        };
        inner.push(String::new());
        let elem = table_elem_rust(t.element);
        inner.push(format!("fn {get}(&self) -> &[{elem}] {{ {borrow} }}"));
        inner.push(format!(
            "fn {get_mut}(&mut self) -> &mut Vec<{elem}> {{ {borrow_mut} }}"
        ));
    }
    Ok(inner)
}

/// The native WASI functions as inherent methods backed by `self.mem()`, each
/// recognised kind emitted once in first-import order.
fn wasi_method_lines(imports: &[ImportInfo], layout: &Layout, shared: bool) -> Vec<String> {
    let wasi_files = layout.wasi_files;
    let mut inner: Vec<String> = Vec::new();
    let mut wasi_emitted: Vec<WasiFn> = Vec::new();
    if imports
        .iter()
        .any(|im| im.wasi.is_some_and(WasiFn::confines_path))
    {
        inner.extend(confine_lines());
    }
    for im in imports {
        if let Some(w) = im.wasi
            && !wasi_emitted.contains(&w)
        {
            wasi_emitted.push(w);
            inner.push(String::new());
            // `thread_spawn_lines` clones the `Arc` itself rather than locking it.
            let body = if w == WasiFn::ThreadSpawn {
                thread_spawn_lines(layout)
            } else if w == WasiFn::PthreadCreateJs {
                pthread_create_js_lines(layout)
            } else {
                w.lines(
                    wasi_files,
                    WasiAccess {
                        shared_memory: shared,
                        locked_fds: layout.wasi_threads && wasi_files,
                    },
                )
            };
            inner.extend(body);
        }
    }
    inner
}

/// `wasi::thread-spawn(start_arg) -> tid`: allocate a thread id (1..=0x1fff_ffff,
/// negative when exhausted or the thread cannot start), then run a fresh sibling
/// instance over the same shared memory on a new thread, entering it through
/// `wasi_thread_start(tid, start_arg)`. The child is detached. A trap in it ends
/// the whole process with a nonzero status, as the proposal requires. The
/// descriptor table is shared: a child sees the descriptors its siblings opened.
fn thread_spawn_lines(layout: &Layout) -> Vec<String> {
    spawn_thread_lines(
        layout,
        "fn wasi_thread_spawn(&mut self, a0: i32) -> i32 {",
        &[
            "let tid = self.wasi_tid.fetch_add(1, std::sync::atomic::Ordering::SeqCst).wrapping_add(1);",
            "if !(1..=0x1fff_ffff).contains(&tid) { return -1; }",
        ],
        &["t.wasi_thread_start(tid, a0);"],
        "\"wasi-threads: thread {tid} trapped; terminating the process\"",
        ("tid", "-1"),
    )
}

/// `env.__pthread_create_js(pthread_ptr, attr, start, arg)`: what an emscripten
/// JS host answers by starting a Worker. Run a fresh sibling instance over the
/// same shared memory on a new thread and replay the Worker's start-up: init the
/// thread with its pthread pointer, init its TLS, call the table entry `start`
/// with `arg`, then exit the thread with the result. The child is detached; a
/// trap in it ends the whole process. Returns 0, or EAGAIN (6) if no thread could
/// be started.
fn pthread_create_js_lines(layout: &Layout) -> Vec<String> {
    spawn_thread_lines(
        layout,
        "fn emscripten_pthread_create_js(&mut self, a0: i32, a1: i32, a2: i32, a3: i32) -> i32 {",
        &[],
        &[
            "t._emscripten_thread_init(a0, 0, 0, 1, 0, 0);",
            "t._emscripten_tls_init();",
            "let r = t.emscripten_call_start(a2 as u32, a3);",
            "t._emscripten_thread_exit(r);",
        ],
        "\"emscripten pthread trapped; terminating the process\"",
        ("0", "6"),
    )
}

/// The shared skeleton of a native thread-starting import: `prelude` runs on the
/// parent, then a detached thread builds a sibling instance (`Self::with_memory`
/// over the shared memory, sharing the tid counter, descriptor table and
/// preopen), runs `run` and, if it unwinds, prints `trap_msg` and exits 134.
/// `ok_err` is the (success, failure) value the import returns.
fn spawn_thread_lines(
    layout: &Layout,
    header: &str,
    prelude: &[&str],
    run: &[&str],
    trap_msg: &str,
    ok_err: (&str, &str),
) -> Vec<String> {
    let mut lines = vec![header.to_string()];
    lines.extend(prelude.iter().map(|l| format!("    {l}")));
    lines.extend(
        [
            "    let mem = self.memory.clone();",
            "    let tids = self.wasi_tid.clone();",
        ]
        .map(str::to_string),
    );
    if layout.has_imports {
        lines.push("    let imports = self.imports.clone();".to_string());
    }
    if layout.wasi_files {
        lines.push("    let fds = self.wasi_fds.clone();".to_string());
    }
    if layout.wasi_preopen {
        lines.push("    let preopen = self.wasi_preopen.clone();".to_string());
    }
    lines.push("    let started = std::thread::Builder::new().spawn(move || {".to_string());
    lines.push(if layout.has_imports {
        "        let mut t = Self::with_memory(mem, imports);".to_string()
    } else {
        "        let mut t = Self::with_memory(mem);".to_string()
    });
    lines.push("        t.wasi_tid = tids;".to_string());
    if layout.wasi_files {
        lines.push("        t.wasi_fds = fds;".to_string());
    }
    if layout.wasi_preopen {
        lines.push("        t.wasi_preopen = preopen;".to_string());
    }
    lines.push("        let run = std::panic::AssertUnwindSafe(|| {".to_string());
    lines.extend(run.iter().map(|l| format!("            {l}")));
    lines.extend(
        [
            "        });",
            "        if std::panic::catch_unwind(run).is_err() {",
        ]
        .map(str::to_string),
    );
    lines.push(format!("            eprintln!({trap_msg});"));
    lines.extend(
        [
            "            std::process::exit(134);",
            "        }",
            "    });",
        ]
        .map(str::to_string),
    );
    lines.push(format!(
        "    if started.is_ok() {{ {} }} else {{ {} }}",
        ok_err.0, ok_err.1
    ));
    lines.push("}".to_string());
    lines
}

/// The used memory-access helper methods, grouped by memory index (0 first, so
/// the historic single-memory helpers keep their position) then in the canonical
/// `HELPER_ORDER` within each memory. A shared module (single memory 0) emits
/// the variants that lock `self.memory.bytes()` instead of borrowing.
fn helper_method_lines(
    used: &HashSet<(Helper, u32)>,
    memories: &[super::MemInfo],
    memory_shared: bool,
    unsafe_memory: bool,
) -> Vec<String> {
    let mut inner: Vec<String> = Vec::new();
    let mut mem_indices: Vec<u32> = used.iter().map(|(_, mem)| *mem).collect();
    mem_indices.sort_unstable();
    mem_indices.dedup();
    for mem in mem_indices {
        for helper in HELPER_ORDER {
            if used.contains(&(helper, mem)) {
                inner.push(String::new());
                if memory_shared {
                    // The shared-memory backend reads `self.memory.bytes()` per
                    // body; unchecked access is not wired through it, so
                    // `unsafe_memory` does not apply to a shared memory.
                    inner.extend(shared_helper_lines(helper));
                } else {
                    // `memories` is the same index-ordered sequence `ctx.memory64`
                    // is derived from; each memory's `memory.grow` is bounded by
                    // its own maximum.
                    let info = memories.get(mem as usize);
                    let is64 = info.is_some_and(|m| m.memory64);
                    let max_pages = info.and_then(|m| m.max_pages);
                    inner.extend(helper_lines(helper, mem, max_pages, unsafe_memory, is64));
                }
            }
        }
    }
    inner
}

/// The export name wasi-threads looks up to start a spawned thread.
const WASI_THREAD_START: &str = "wasi_thread_start";

/// The exports an emscripten pthread's start-up drives, with their signatures:
/// `_emscripten_thread_init(ptr, is_main, is_runtime, can_block, 0, 0)`,
/// `_emscripten_tls_init() -> i32` and `_emscripten_thread_exit(result)`.
const EMSCRIPTEN_ENTRIES: [(&str, &[ValType], &[ValType]); 3] = {
    use ValType::I32;
    [
        (
            "_emscripten_thread_init",
            &[I32, I32, I32, I32, I32, I32],
            &[],
        ),
        ("_emscripten_tls_init", &[], &[I32]),
        ("_emscripten_thread_exit", &[I32], &[]),
    ]
};

/// The export names the generated code itself calls into, and whether the
/// module also reads the table through them (`emscripten_call_start` calls a
/// table entry by index). Used to keep them live under `entry_exports`.
pub(super) fn internal_entry_names(imports: &[ImportInfo]) -> (Vec<&'static str>, bool) {
    let imports_wasi = |f: WasiFn| imports.iter().any(|im| im.wasi == Some(f));
    let mut names = vec![WASI_THREAD_START];
    if imports_wasi(WasiFn::PthreadCreateJs) || imports_wasi(WasiFn::EmscriptenInitMainThread) {
        names.extend(EMSCRIPTEN_ENTRIES.iter().map(|(name, ..)| *name));
    }
    (names, imports_wasi(WasiFn::PthreadCreateJs))
}

/// By-name entry points forwarding to the exported functions' `func{N}`: always
/// `wasi_thread_start` when exported, and, for a module importing
/// `env.__pthread_create_js`, the emscripten thread start-up exports plus
/// `emscripten_call_start`, which calls a table entry `(i32) -> i32` by index
/// (the Worker's `getWasmTableEntry(start)(arg)`). Any other export keeps only
/// its `func{N}` name, so ordinary modules are emitted unchanged. A stateful
/// module gets methods, a stateless one free functions.
pub(super) fn entry_point_lines(
    parts: &ModuleParts<'_>,
    ctx: &ModuleCtx<'_>,
) -> Result<Vec<String>, TranspileError> {
    let mut lines = Vec::new();
    if parts.exports.iter().any(|e| e.name == WASI_THREAD_START) {
        lines.extend(named_entry(
            parts,
            ctx,
            WASI_THREAD_START,
            &[ValType::I32, ValType::I32],
            &[],
        )?);
    }
    let imports_wasi = |f: WasiFn| parts.imports.iter().any(|im| im.wasi == Some(f));
    if imports_wasi(WasiFn::PthreadCreateJs) || imports_wasi(WasiFn::EmscriptenInitMainThread) {
        for (name, params, results) in EMSCRIPTEN_ENTRIES {
            if !parts.exports.iter().any(|e| e.name == name) {
                return Err(TranspileError::Unsupported(format!(
                    "emscripten thread imports require an exported {name}"
                )));
            }
            lines.extend(named_entry(parts, ctx, name, params, results)?);
        }
        if imports_wasi(WasiFn::PthreadCreateJs) {
            lines.extend(call_start_lines(ctx)?);
        }
    }
    if parts.opts.named_exports {
        lines.extend(named_export_lines(parts, ctx)?);
    }
    Ok(lines)
}

/// A by-name wrapper forwarding to `func{N}` for every exported function that is
/// defined (not imported) and emitted, with the same signature. Names come from
/// [`super::export_names::assign`]; `wasi_thread_start` already has its own entry
/// point, so it is skipped here.
fn named_export_lines(
    parts: &ModuleParts<'_>,
    ctx: &ModuleCtx<'_>,
) -> Result<Vec<String>, TranspileError> {
    let exports: Vec<&ExportInfo> = parts
        .exports
        .iter()
        .filter(|e| e.name != WASI_THREAD_START)
        .collect();
    // Global getters and setters share the namespace with the function wrappers, so
    // the names are assigned together (functions, then getters, then setters; an
    // earlier name wins a clash, so a setter never displaces an export).
    let settable = settable_globals(parts, ctx);
    let setter_raw: Vec<String> = settable
        .iter()
        .map(|(e, _)| format!("set_{}", e.name))
        .collect();
    let idents = super::export_names::assign(
        exports
            .iter()
            .map(|e| (e.name.as_str(), e.func_index))
            .chain(
                parts
                    .global_exports
                    .iter()
                    .map(|g| (g.name.as_str(), g.global_index)),
            )
            .chain(
                settable
                    .iter()
                    .zip(&setter_raw)
                    .map(|((e, _), raw)| (raw.as_str(), e.global_index)),
            ),
    );
    let getters_end = exports.len() + parts.global_exports.len();
    let mut lines = Vec::new();
    lines.extend(global_getter_lines(
        parts,
        ctx,
        &idents[exports.len()..getters_end],
    ));
    lines.extend(global_setter_lines(&settable, &idents[getters_end..]));
    for (export, ident) in exports.iter().zip(idents) {
        let Some(defined) = usize::try_from(export.func_index)
            .ok()
            .and_then(|i| i.checked_sub(parts.imports.len()))
        else {
            continue;
        };
        let (Some(sig), Some(&live)) = (parts.funcs.get(defined), parts.live.get(defined)) else {
            continue;
        };
        if !live {
            continue;
        }
        let mut decls: Vec<String> = Vec::new();
        if ctx.is_method {
            decls.push("&mut self".to_string());
        }
        let mut args: Vec<String> = Vec::new();
        for (k, ty) in sig.params.iter().enumerate() {
            decls.push(format!("a{k}: {}", rust_type(*ty, ctx.type_kinds)?));
            args.push(format!("a{k}"));
        }
        let ret = match sig.results {
            [] => String::new(),
            [ty] => format!(" -> {}", rust_type(*ty, ctx.type_kinds)?),
            many => format!(" -> ({})", rust_types(many, ctx.type_kinds)?.join(", ")),
        };
        let call = ctx.call_expr(export.func_index as usize, &args.join(", "));
        lines.push(String::new());
        lines.push(format!(
            "pub fn {ident}({}){ret} {{ {call} }}",
            decls.join(", ")
        ));
    }
    Ok(lines)
}

/// An exported numeric global resolved against the module's globals.
struct NumericGlobal {
    index: usize,
    rust_ty: &'static str,
    imported: bool,
    mutable: bool,
}

/// The numeric (`i32`/`i64`/`f32`/`f64`) global with the given index, or `None`
/// for a reference-typed or `v128` one.
fn numeric_global(ctx: &ModuleCtx<'_>, index: usize) -> Option<NumericGlobal> {
    let (ty, mutable, imported) = match index.checked_sub(ctx.imported_globals.len()) {
        None => {
            let &(ty, mutable) = ctx.imported_globals.get(index)?;
            (ty, mutable, true)
        }
        Some(defined) => {
            let &(ty, mutable) = ctx.globals.get(defined)?;
            (ty, mutable, false)
        }
    };
    let rust_ty = match ty {
        ValType::I32 => "i32",
        ValType::I64 => "i64",
        ValType::F32 => "f32",
        ValType::F64 => "f64",
        _ => return None,
    };
    Some(NumericGlobal {
        index,
        rust_ty,
        imported,
        mutable,
    })
}

/// A `pub fn <name>(&self) -> T` getter for every exported numeric global (a
/// reference-typed or `v128` one is skipped), reading the current value: the field
/// of a defined global, the host's `get_global{k}` for an imported one. `idents`
/// are the names assigned to `parts.global_exports`, in order. A global only
/// exists in a stateful module, so a stateless one has no getters.
fn global_getter_lines(
    parts: &ModuleParts<'_>,
    ctx: &ModuleCtx<'_>,
    idents: &[String],
) -> Vec<String> {
    let mut lines = Vec::new();
    if !ctx.is_method {
        return lines;
    }
    for (export, ident) in parts.global_exports.iter().zip(idents) {
        let Some(g) = numeric_global(ctx, export.global_index as usize) else {
            continue;
        };
        let (index, rust_ty) = (g.index, g.rust_ty);
        let read = if g.imported {
            format!("self.imports.get_global{index}()")
        } else {
            format!("self.g{index}")
        };
        lines.push(String::new());
        lines.push(format!("pub fn {ident}(&self) -> {rust_ty} {{ {read} }}"));
    }
    lines
}

/// The exported mutable numeric globals that get a setter, each with its export
/// (whose name the setter's is derived from) and its resolved global. A stateless
/// module has no globals, so none.
fn settable_globals<'a>(
    parts: &'a ModuleParts<'_>,
    ctx: &ModuleCtx<'_>,
) -> Vec<(&'a GlobalExportInfo, NumericGlobal)> {
    if !ctx.is_method {
        return Vec::new();
    }
    parts
        .global_exports
        .iter()
        .filter_map(|e| numeric_global(ctx, e.global_index as usize).map(|g| (e, g)))
        .filter(|(_, g)| g.mutable)
        .collect()
}

/// A `pub fn <name>(&mut self, v: T)` setter for every exported mutable numeric
/// global, writing the field of a defined global or the host's `set_global{k}` for
/// an imported one. `idents` are the setter names, in `settable` order.
fn global_setter_lines(
    settable: &[(&GlobalExportInfo, NumericGlobal)],
    idents: &[String],
) -> Vec<String> {
    let mut lines = Vec::new();
    for ((_, g), ident) in settable.iter().zip(idents) {
        let (index, rust_ty) = (g.index, g.rust_ty);
        let write = if g.imported {
            format!("self.imports.set_global{index}(v)")
        } else {
            format!("self.g{index} = v")
        };
        lines.push(String::new());
        lines.push(format!(
            "pub fn {ident}(&mut self, v: {rust_ty}) {{ {write} }}"
        ));
    }
    lines
}

/// One by-name wrapper for the export `name`, whose function must be defined
/// (not imported) and have exactly the given signature.
fn named_entry(
    parts: &ModuleParts<'_>,
    ctx: &ModuleCtx<'_>,
    name: &str,
    params: &[ValType],
    results: &[ValType],
) -> Result<Vec<String>, TranspileError> {
    let Some(export) = parts.exports.iter().find(|e| e.name == name) else {
        return Ok(Vec::new());
    };
    let unsupported = |why: &str| TranspileError::Unsupported(format!("{name}: {why}"));
    let sig = usize::try_from(export.func_index)
        .ok()
        .and_then(|i| i.checked_sub(parts.imports.len()))
        .and_then(|i| parts.funcs.get(i))
        .ok_or_else(|| unsupported("must be a defined function, not an import"))?;
    if sig.params != params || sig.results != results {
        return Err(unsupported(&format!(
            "must have type {params:?} -> {results:?}"
        )));
    }
    let self_param = if ctx.is_method { "&mut self" } else { "" };
    let arg_decls: Vec<String> = (0..params.len()).map(|k| format!("a{k}: i32")).collect();
    let arg_list: Vec<String> = (0..params.len()).map(|k| format!("a{k}")).collect();
    let sep = if ctx.is_method && !arg_decls.is_empty() {
        ", "
    } else {
        ""
    };
    let ret = if results.is_empty() { "" } else { " -> i32" };
    let call = ctx.call_expr(export.func_index as usize, &arg_list.join(", "));
    Ok(vec![
        String::new(),
        format!(
            "pub fn {name}({self_param}{sep}{}){ret} {{ {call} }}",
            arg_decls.join(", ")
        ),
    ])
}

/// `emscripten_call_start(f, a0) -> i32`: look `f` up in the table and call the
/// `(i32) -> i32` function it names, panicking (a trap) on null or a wrong type. Needs a
/// module-owned table, since the start routine is a table index.
fn call_start_lines(ctx: &ModuleCtx<'_>) -> Result<Vec<String>, TranspileError> {
    if !ctx.is_method {
        return Err(TranspileError::Unsupported(
            "env.__pthread_create_js requires a stateful module".into(),
        ));
    }
    let mut lines = vec![
        String::new(),
        "pub fn emscripten_call_start(&mut self, f: u32, a0: i32) -> i32 {".to_string(),
        "    match self.table().get(f as usize).copied().unwrap_or(u32::MAX) {".to_string(),
    ];
    for &fidx in ctx.funcs_with_sig(&[ValType::I32], &[ValType::I32]) {
        // A continuation step function is `cont_step_func{N}`, not `func{N}`.
        if u32::try_from(fidx).is_ok_and(|f| ctx.cont.step_set.binary_search(&f).is_ok()) {
            continue;
        }
        lines.push(format!(
            "        {fidx}u32 => {},",
            ctx.call_expr(fidx, "a0")
        ));
    }
    lines.push(
        "        _ => panic!(\"emscripten pthread: start routine is not a (i32) -> i32 function\"),"
            .to_string(),
    );
    lines.push("    }".to_string());
    lines.push("}".to_string());
    Ok(lines)
}

/// One public `call_ref_t{ti}` dispatch method per `call_indirect` signature. It
/// is both the target of the module's own `call_indirect` and the entry point
/// through which the host invokes a funcref the module handed out.
fn dispatch_method_block(
    ctx: &ModuleCtx<'_>,
    dispatch_sigs: &HashSet<u32>,
    has_imports: bool,
) -> Result<Vec<String>, TranspileError> {
    let mut ordered: Vec<u32> = dispatch_sigs.iter().copied().collect();
    ordered.sort_unstable();
    let mut inner: Vec<String> = Vec::new();
    for ti in ordered {
        inner.push(String::new());
        inner.extend(dispatch_method_lines(ctx, ti, has_imports, false)?);
        if ctx.has_tc_dispatch(ti) {
            inner.push(String::new());
            inner.extend(dispatch_method_lines(ctx, ti, has_imports, true)?);
        }
    }
    Ok(inner)
}

/// The module-scope runtime for a `shared` memory: the thread-shareable
/// `SharedMemory` handle (a `#[derive(Clone)]` `Arc`, so it is `Send + Sync`),
/// the combine helper, and the atomic load/store/RMW/cmpxchg/wait/notify
/// methods. Emitted verbatim (warning-clean under `-D warnings`) and used only
/// when the module has a single defined shared memory.
///
/// The bytes are reserved once, up to the memory's maximum, and never move, so
/// threads read and write them without a lock (`memory.grow` only raises the
/// current length). A plain access is an ordinary, non-atomic slice access: as
/// in wasm, two threads racing on the same bytes see unspecified values, which
/// Rust formally calls a data race. Atomic operations are real atomic
/// instructions (sequentially consistent) and trap on a misaligned address.
fn shared_memory_runtime_lines() -> Vec<String> {
    include_str!("rt_src/shared_memory.rs")
        .lines()
        .map(|l| l.to_string())
        .collect()
}

/// Whether the module needs the injected `Imports` host trait — i.e. it has a
/// non-WASI function import, an imported global, or host-owned memory/table.
/// Recognised WASI imports are native inherent methods and need no host, so a
/// module whose imports are all WASI is fully standalone.
fn needs_host_trait(parts: &ModuleParts<'_>) -> bool {
    parts.imports.iter().any(|im| im.wasi.is_none())
        || !parts.imported_globals.is_empty()
        || parts.memories.iter().any(|m| m.imported && !m.shared)
        || parts.table.is_some_and(|t| t.imported)
}

/// The generic-parameter fragments for the `Instance` type: `<H: Imports>` where
/// the parameter is bound (struct/impl headers) and `<H>` where the type is
/// merely named (`Instance<H>`), or empty strings when no host is needed.
fn host_generics(parts: &ModuleParts<'_>) -> (&'static str, &'static str) {
    if !needs_host_trait(parts) {
        return ("", "");
    }
    // `wasi::thread-spawn` hands each thread its own clone of the host.
    if parts
        .imports
        .iter()
        .any(|im| im.wasi.is_some_and(WasiFn::spawns_threads))
    {
        ("<H: Imports + Clone + Send + 'static>", "<H>")
    } else {
        ("<H: Imports>", "<H>")
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
        render_module(&mut out, parts, ctx, &[], deps.helpers, deps.dispatch_sigs)?;
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
        for line in entry_point_lines(parts, ctx)? {
            out.push_str(&line);
            out.push('\n');
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
    tc: bool,
) -> Result<Vec<String>, TranspileError> {
    let sig = ctx
        .types
        .get(type_index as usize)
        .ok_or_else(|| TranspileError::Unsupported("call_indirect: unknown type".into()))?;
    let targets = ctx.dispatch_targets(type_index);

    let mut params = String::from("&mut self, f: u32");
    for (k, ty) in sig.params.iter().enumerate() {
        params.push_str(&format!(", a{k}: {}", rust_type(*ty, ctx.type_kinds)?));
    }
    if tc {
        params.push_str(&format!(", __tc: &mut {}", ctx.tc_type(&sig.results)?));
    }
    let name = if tc {
        format!("call_ref_t{type_index}_tc")
    } else {
        format!("call_ref_t{type_index}")
    };
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
        format!("pub fn {name}({params}){ret} {{"),
        "    match f {".to_string(),
    ];
    for &fidx in targets {
        let expr = if tc && ctx.has_tc_variant(fidx) {
            format!("self.func{fidx}_tc({arg_list}{sep}__tc)")
        } else {
            ctx.call_expr(fidx, &arg_list)
        };
        lines.push(format!("        {fidx}u32 => {expr},"));
    }
    // The high bit tags an external handle the host resolves (see the trait's
    // defaulted `call_ref_t{ti}`); the tag is stripped before the host sees the
    // slot. `u32::MAX` (null) has the high bit set too, so it is matched first
    // and traps rather than being forwarded. Only a module with a host (an
    // `Imports` trait) can receive external handles.
    if has_imports {
        lines.push("        u32::MAX => trap_indirect_type_mismatch(),".to_string());
        let slot_and_args = format!("h & {EXTERNAL_HANDLE_SLOT_MASK}{sep}{arg_list}");
        let call = ctx.host_call(
            &format!("call_ref_t{type_index}"),
            sig.params.len() + 1,
            &slot_and_args,
        );
        lines.push(format!(
            "        h if (h & {EXTERNAL_HANDLE_BIT}) != 0 => {call},"
        ));
    }
    lines.push("        _ => trap_indirect_type_mismatch(),".to_string());
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
        lines.push(format!("        {EXTERNAL_HANDLE_BIT} | slot"));
        lines.push("    }".to_string());
        lines.push(format!("    pub fn call({call_params}){ret} {{"));
        lines.push(format!("        (self.fns[slot as usize])({arg_list})"));
        lines.push("    }".to_string());
        lines.push("}".to_string());
    }
    Ok(lines)
}

/// The `pub trait Imports` declaration: one `import{j}` (or, with `named_imports`,
/// name-derived) method per imported
/// function, taking `&mut self` since a host call may have side effects.
fn import_trait_lines(
    ctx: &ModuleCtx<'_>,
    imports: &[ImportInfo],
    imported_globals: &[ImportedGlobalInfo],
    mem_imported: bool,
    imported_tables: usize,
    dispatch_sigs: &HashSet<u32>,
) -> Result<Vec<String>, TranspileError> {
    let reentrant = ctx.reentrant_imports;
    let mut lines = vec![if reentrant {
        "pub trait Imports: Sized {".to_string()
    } else {
        "pub trait Imports {".to_string()
    }];
    // A re-entrant method takes the instance right after `self`.
    let self_params = if reentrant {
        "&mut self, g: &mut Instance<Self>"
    } else {
        "&mut self"
    };
    // Imported memory: the host lends its buffer for loads/stores/grow.
    if mem_imported {
        lines.push("    fn memory(&self) -> &[u8];".to_string());
        lines.push("    fn memory_mut(&mut self) -> &mut Vec<u8>;".to_string());
    }
    // Imported tables: the host lends each `Vec<u32>` (`Vec<GcRef>` for an
    // `exnref` table) for get/set/size/grow and for indirect-call dispatch (each
    // `u32` entry is a function index). Imports come
    // first in the index space, so they are tables `0..imported_tables`.
    for i in 0..imported_tables {
        let (get, get_mut) = table_accessor(index_u32(i)?);
        let elem = table_elem_rust(
            ctx.table_elements
                .get(i)
                .copied()
                .unwrap_or(ValType::FUNCREF),
        );
        lines.push(format!("    fn {get}(&self) -> &[{elem}];"));
        lines.push(format!("    fn {get_mut}(&mut self) -> &mut Vec<{elem}>;"));
    }
    for (j, im) in imports.iter().enumerate() {
        // A recognised WASI import is a native inherent method, not a host
        // trait method; skip it while keeping `import{j}` aligned to the
        // absolute import index used by `call_expr`.
        if im.wasi.is_some() {
            continue;
        }
        let mut params = String::from(self_params);
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
        let name = im.ident.clone().unwrap_or_else(|| format!("import{j}"));
        lines.push(format!("    fn {name}({params}){ret};"));
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
        let mut params = if reentrant {
            String::from("&mut self, _g: &mut Instance<Self>, _slot: u32")
        } else {
            String::from("&mut self, _slot: u32")
        };
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

/// The shared-memory runtime is emitted verbatim into generated crates; compiling
/// it here puts it under rustc and lets it be unit-tested directly.
#[cfg(test)]
#[allow(
    dead_code,
    unsafe_code,
    clippy::all,
    clippy::pedantic,
    clippy::restriction,
    reason = "emitted verbatim; helpers unused by some modules"
)]
mod shared_memory_rt {
    include!("rt_src/shared_memory.rs");

    #[test]
    fn grow_raises_length_up_to_the_maximum_and_reports_old_pages() {
        let mem = SharedMemory::with_max(65536, 3 * 65536);
        assert_eq!(mem.grow(1), 1);
        assert_eq!(mem.grow(1), 2);
        assert_eq!(mem.grow(1), -1);
    }

    #[test]
    fn atomic_rmw_wraps_at_the_access_width() {
        let mem = SharedMemory::new(65536);
        mem.atomic_store(0, 1, 0xff);
        assert_eq!(mem.atomic_rmw(0, 1, 0, 2), 0xff);
        assert_eq!(mem.atomic_load(0, 1), 1);
    }

    #[test]
    fn cmpxchg_stores_only_on_match() {
        let mem = SharedMemory::new(65536);
        mem.atomic_store(8, 4, 7);
        assert_eq!(mem.atomic_cmpxchg(8, 4, 6, 9), 7);
        assert_eq!(mem.atomic_load(8, 4), 7);
        assert_eq!(mem.atomic_cmpxchg(8, 4, 7, 9), 7);
        assert_eq!(mem.atomic_load(8, 4), 9);
    }

    // Generated helpers each bind `bytes()` once, and a `memory.copy` / host
    // data init can hold more than one view at a time; none may be a whole-memory
    // `&mut [u8]` (UB under Stacked Borrows, checked by `cargo +nightly miri test`).
    #[test]
    fn simultaneous_views_do_not_alias() {
        let mem = SharedMemory::new(65536);
        let mut a = mem.bytes();
        let mut b = mem.bytes();
        a[0] = 1;
        b[1] = 2;
        a[0] = 3;
        b[2..4].copy_from_slice(&[4, 5]);
        assert_eq!((a[0], a[1], b[2], b[3]), (3, 2, 4, 5));
    }

    #[test]
    fn plain_access_interleaves_with_atomics() {
        let mem = SharedMemory::new(65536);
        let mut m = mem.bytes();
        m[0..4].copy_from_slice(&7u32.to_le_bytes());
        assert_eq!(mem.atomic_load(0, 4), 7);
        mem.atomic_store(4, 4, 9);
        assert_eq!(u32::from_le_bytes(m[4..8].try_into().unwrap()), 9);
        m[0] = 1;
        assert_eq!(mem.atomic_rmw(0, 4, 0, 1), 1);
        assert_eq!(m[0], 2);
    }

    #[test]
    fn copy_within_moves_overlapping_ranges_and_traps_out_of_bounds() {
        let mem = SharedMemory::new(65536);
        let mut m = mem.bytes();
        m[0..4].copy_from_slice(&[1, 2, 3, 4]);
        m.copy_within(0..4, 2);
        assert_eq!(&m[0..6], &[1, 2, 1, 2, 3, 4]);
        assert!(std::panic::catch_unwind(move || m.copy_within(0..4, 65534)).is_err());
    }

    #[test]
    fn view_out_of_bounds_panics() {
        let mem = SharedMemory::new(65536);
        let m = mem.bytes();
        assert!(std::panic::catch_unwind(|| m[65536]).is_err());
        assert!(std::panic::catch_unwind(|| m[65530..65537].len()).is_err());
        assert!(std::panic::catch_unwind(|| m[usize::MAX..usize::MAX].len()).is_err());
    }

    #[test]
    fn wait_times_out_and_mismatch_returns_not_equal() {
        let mem = SharedMemory::new(65536);
        assert_eq!(mem.wait(0, 1, 4, 0), 1);
        assert_eq!(mem.wait(0, 0, 4, 1_000), 2);
    }
}

#[cfg(test)]
mod tests {
    use super::push_function_source;

    #[test]
    fn function_source_is_blank_line_separated_and_indented_in_place() {
        let mut out = String::from("impl Instance {\n");
        push_function_source(&mut out, "fn a() {\n\n    1\n}\n");
        push_function_source(&mut out, "fn b() {}");
        assert_eq!(
            out,
            "impl Instance {\n\n    fn a() {\n\n        1\n    }\n\n    fn b() {}\n"
        );
    }
}
