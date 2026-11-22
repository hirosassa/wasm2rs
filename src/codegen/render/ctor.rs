//! The `Instance` constructors: field initialisers, data/element segment copies and start-time setup.

use super::Layout;
use super::statics::TrimmedData;
use crate::TranspileError;
use crate::codegen::helpers::table_accessor;
use crate::codegen::{
    DataSegment, ElemSegment, ModuleCtx, ModuleParts, SegOffset, TableFill, WASM_PAGE_SIZE,
    index_u32, is_gc_table_element, table_elem_rust, table_null,
};

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
    active: Vec<(usize, &'a DataSegment<'a>)>,
    active_elem: Vec<&'a ElemSegment>,
    /// The zero-trimmed copy of each data segment that has one (see
    /// [`trimmed_data`](super::statics::trimmed_data)), indexed like `parts.data`; the same one the segment
    /// statics were sized from.
    trimmed: &'a [Option<TrimmedData<'a>>],
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
    fn new(
        parts: &'a ModuleParts<'a>,
        ctx: &'a ModuleCtx<'a>,
        layout: &'a Layout,
        trimmed: &'a [Option<TrimmedData<'a>>],
    ) -> Self {
        let active: Vec<(usize, &DataSegment<'_>)> = parts
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
            && !ctx.memory.shared
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
            trimmed,
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
    fn active_for(&self, mem: u32) -> Vec<(usize, &'a DataSegment<'a>)> {
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
        self.ctx.memory.shared
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
        seg: &DataSegment<'_>,
        trimmed: Option<&TrimmedData<'_>>,
        target: &str,
        host: &str,
        indent: &str,
    ) {
        let (base, runtime) = SegOffset::split(seg.offset.as_ref());
        // A trimmed segment is in bounds and has a constant offset, so skipping
        // its zero edges (or the whole copy) cannot change a trap or a byte.
        let (base, len) = match trimmed {
            Some(t) if t.bytes.is_empty() => return,
            Some(t) => (
                base.saturating_add(u64::try_from(t.start).unwrap_or(u64::MAX)),
                u64::try_from(t.bytes.len()).unwrap_or(u64::MAX),
            ),
            None => (base, u64::try_from(seg.bytes.len()).unwrap_or(u64::MAX)),
        };
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
            let trimmed = self.trimmed.get(d).and_then(Option::as_ref);
            Self::copy_data_seg(lines, d, seg, trimmed, target, host, indent);
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
            .table
            .elements
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
                    "        wasi_fds: std::sync::Arc::new(std::sync::Mutex::new(WasiFds::default())),"
                } else {
                    "        wasi_fds: WasiFds::default(),"
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
                    let trimmed = self.trimmed.get(*d).and_then(Option::as_ref);
                    Self::copy_data_seg(
                        inner,
                        *d,
                        seg,
                        trimmed,
                        &target,
                        "instance.imports",
                        "    ",
                    );
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
pub(super) fn constructor_lines(
    parts: &ModuleParts<'_>,
    ctx: &ModuleCtx<'_>,
    layout: &Layout,
    trimmed: &[Option<TrimmedData<'_>>],
) -> Result<Vec<String>, TranspileError> {
    let plan = CtorPlan::new(parts, ctx, layout, trimmed);
    let mut inner: Vec<String> = Vec::new();
    if ctx.memory.shared {
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
pub(super) fn with_host_lines() -> Vec<String> {
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
