use std::collections::HashSet;

use super::helpers::{HELPER_ORDER, helper_lines};
use super::runtime::render_rt_helpers;
use super::{
    ALLOW, DataSegment, ElemSegment, Helper, ImportInfo, ImportedGlobalInfo, ModuleCtx,
    ModuleParts, WasiFn, byte_array_literal, indent, rust_type, rust_types,
};
use crate::TranspileError;

/// Render the `struct Instance` and its `impl` for a stateful module. When the
/// module imports functions, a `pub trait Imports` is emitted and `Instance`
/// becomes generic over a host `H: Imports` that it stores and dispatches to.
pub(super) fn render_module(
    parts: &ModuleParts<'_>,
    ctx: &ModuleCtx<'_>,
    sources: &[String],
    used: &HashSet<Helper>,
    dispatch_sigs: &HashSet<u32>,
) -> Result<String, TranspileError> {
    let ModuleParts {
        imports,
        imported_globals,
        globals,
        memory,
        data,
        table,
        elements,
        ..
    } = *parts;

    // Defined globals are named by their full index, i.e. after the imported
    // globals in the shared global index space.
    let global_base = imported_globals.len();

    let mut lines: Vec<String> = Vec::new();

    let mem_imported = memory.is_some_and(|m| m.imported);
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

    lines.push("#[allow(dead_code)]".to_string());
    lines.push(format!("pub struct Instance{decl_generics} {{"));
    if has_imports {
        lines.push("    imports: H,".to_string());
    }
    // Imported memory lives in the host, so the instance owns no buffer.
    if memory.is_some() && !mem_imported {
        lines.push("    memory: Vec<u8>,".to_string());
    }
    // Imported tables live in the host, so the instance owns no storage.
    if table.is_some() && !table_imported {
        // A table entry is a function index; `u32::MAX` marks a null funcref.
        lines.push("    table: Vec<u32>,".to_string());
    }
    for (i, g) in globals.iter().enumerate() {
        lines.push(format!("    g{}: {},", global_base + i, rust_type(g.ty)?));
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
    lines.push("}".to_string());
    lines.push(String::new());

    lines.push(ALLOW.to_string());
    lines.push(format!("impl{decl_generics} Instance{type_generics} {{"));

    // Everything inside the `impl` is collected unindented, then indented once.
    let mut inner: Vec<String> = Vec::new();
    let new_param = if has_imports { "imports: H" } else { "" };
    // Only active segments are copied at instantiation; passive ones are
    // retained (see the `data{d}` fields) for `memory.init`.
    let active: Vec<&DataSegment> = data.iter().filter(|d| d.offset.is_some()).collect();
    let active_elem: Vec<&ElemSegment> = elements.iter().filter(|e| e.offset.is_some()).collect();
    // Imported memory/table are host-owned, so their active data/elements cannot
    // be written into a `memory`/`table` field literal; instead the instance is
    // bound and the segments are copied into the host storage through
    // `mem_mut()`/`table_mut()` after construction.
    let post_init_data = mem_imported && !active.is_empty();
    let post_init_elem = table_imported && !active_elem.is_empty();
    let post_init = post_init_data || post_init_elem;
    let (open, close) = if post_init {
        ("    let mut instance = Self {", "    };")
    } else {
        ("    Self {", "    }")
    };

    // Emit `<target>[off..end].copy_from_slice(&[bytes]);` for each active data
    // segment; `target` is a defined buffer (`m`) or the host buffer accessor
    // (`instance.mem_mut()`), and `indent` matches the surrounding block.
    let copy_active_data = |lines: &mut Vec<String>, target: &str, indent: &str| {
        for seg in &active {
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

    inner.push(format!("pub fn new({new_param}) -> Self {{"));
    inner.push(open.to_string());
    if has_imports {
        inner.push("        imports,".to_string());
    }
    if let Some(m) = memory.filter(|_| !mem_imported) {
        let bytes = m
            .min_pages
            .checked_mul(65536)
            .ok_or_else(|| TranspileError::Unsupported("memory too large".into()))?;
        if active.is_empty() {
            inner.push(format!("        memory: vec![0u8; {bytes}],"));
        } else {
            // Zero the memory, then copy each active data segment into place.
            inner.push("        memory: {".to_string());
            inner.push(format!(
                "            let mut m: Vec<u8> = vec![0u8; {bytes}];"
            ));
            copy_active_data(&mut inner, "m", "            ");
            inner.push("            m".to_string());
            inner.push("        },".to_string());
        }
    }
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
            copy_active_elems(&mut inner, "t", "            ");
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
    inner.push(close.to_string());
    if post_init {
        // Copy each active data/element segment into the host-owned storage in
        // order. An out-of-bounds write panics here, faithfully reproducing a
        // wasm instantiation trap when a segment does not fit the host storage.
        if post_init_data {
            copy_active_data(&mut inner, "instance.mem_mut()", "    ");
        }
        if post_init_elem {
            copy_active_elems(&mut inner, "instance.table_mut()", "    ");
        }
        inner.push("    instance".to_string());
    }
    inner.push("}".to_string());

    // Uniform memory accessors so the load/store/bulk helpers are identical for
    // defined and imported memory: a defined buffer is a field, an imported one
    // is lent by the host through the `Imports` trait.
    if let Some(m) = memory {
        let (borrow, borrow_mut) = if m.imported {
            ("self.imports.memory()", "self.imports.memory_mut()")
        } else {
            ("&self.memory", "&mut self.memory")
        };
        inner.push(String::new());
        inner.push(format!("fn mem(&self) -> &[u8] {{ {borrow} }}"));
        inner.push(format!(
            "fn mem_mut(&mut self) -> &mut Vec<u8> {{ {borrow_mut} }}"
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

    for helper in HELPER_ORDER {
        if used.contains(&helper) {
            inner.push(String::new());
            inner.extend(helper_lines(helper));
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

/// Whether the module needs the injected `Imports` host trait — i.e. it has a
/// non-WASI function import, an imported global, or host-owned memory/table.
/// Recognised WASI imports are native inherent methods and need no host, so a
/// module whose imports are all WASI is fully standalone.
fn needs_host_trait(parts: &ModuleParts<'_>) -> bool {
    parts.imports.iter().any(|im| im.wasi.is_none())
        || !parts.imported_globals.is_empty()
        || parts.memory.is_some_and(|m| m.imported)
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
    pub(super) helpers: &'a HashSet<Helper>,
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
        params.push_str(&format!(", a{k}: {}", rust_type(*ty)?));
    }
    let ret = match sig.results.as_slice() {
        [] => String::new(),
        [ty] => format!(" -> {}", rust_type(*ty)?),
        many => format!(" -> ({})", rust_types(many)?.join(", ")),
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
        let param_tys = rust_types(&sig.params)?.join(", ");
        let ret = match sig.results.as_slice() {
            [] => String::new(),
            [ty] => format!(" -> {}", rust_type(*ty)?),
            many => format!(" -> ({})", rust_types(many)?.join(", ")),
        };
        let boxed = format!("Box<dyn FnMut({param_tys}){ret}>");
        let mut call_params = String::from("&mut self, slot: u32");
        for (k, ty) in sig.params.iter().enumerate() {
            call_params.push_str(&format!(", a{k}: {}", rust_type(*ty)?));
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
            params.push_str(&format!(", a{k}: {}", rust_type(*ty)?));
        }
        let ret = match im.results.as_slice() {
            [] => String::new(),
            [ty] => format!(" -> {}", rust_type(*ty)?),
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
        let ty = rust_type(g.ty)?;
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
            [ty] => format!(" -> {}", rust_type(*ty)?),
            many => format!(" -> ({})", rust_types(many)?.join(", ")),
        };
        // Underscore the parameters: the default body ignores them, and an
        // override is free to rename them.
        let mut params = String::from("&mut self, _slot: u32");
        for (k, ty) in sig.params.iter().enumerate() {
            params.push_str(&format!(", _a{k}: {}", rust_type(*ty)?));
        }
        lines.push(format!(
            "    fn call_ref_t{ti}({params}){ret} \
             {{ panic!(\"unresolved external funcref\") }}"
        ));
    }
    lines.push("}".to_string());
    Ok(lines)
}
