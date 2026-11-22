//! The host-import surface: the `Imports` trait, the extern registry and dispatch methods.

use crate::TranspileError;
use crate::codegen::helpers::table_accessor;
use crate::codegen::{
    ImportInfo, ImportedGlobalInfo, ModuleCtx, ModuleParts, WasiFn, index_u32, rust_type,
    rust_types, table_elem_rust,
};
use std::collections::HashSet;
use wasmparser::ValType;

/// High bit tagging a funcref handle as an *external* (host-provided) callback
/// rather than one of the module's own function indices. Emitted verbatim into
/// generated code, so it is the underscored literal token, not a `u32`.
const EXTERNAL_HANDLE_BIT: &str = "0x8000_0000u32";

/// Complement of [`EXTERNAL_HANDLE_BIT`]: masks a tagged handle back to the raw
/// external slot before the host resolves it.
const EXTERNAL_HANDLE_SLOT_MASK: &str = "0x7fff_ffffu32";

/// One public `call_ref_t{ti}` dispatch method per `call_indirect` signature. It
/// is both the target of the module's own `call_indirect` and the entry point
/// through which the host invokes a funcref the module handed out.
pub(super) fn dispatch_method_block(
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

/// Whether the module needs the injected `Imports` host trait — i.e. it has a
/// non-WASI function import, an imported global, or host-owned memory/table.
/// Recognised WASI imports are native inherent methods and need no host, so a
/// module whose imports are all WASI is fully standalone.
pub(crate) fn needs_host_trait(parts: &ModuleParts<'_>) -> bool {
    parts.imports.iter().any(ImportInfo::is_host)
        || !parts.tail_imports.is_empty()
        || !parts.imported_globals.is_empty()
        || parts.memories.iter().any(|m| m.imported && !m.shared)
        || parts.table.is_some_and(|t| t.imported)
}

/// The generic-parameter fragments for the `Instance` type: `<H: Imports>` where
/// the parameter is bound (struct/impl headers) and `<H>` where the type is
/// merely named (`Instance<H>`), or empty strings when no host is needed.
pub(super) fn host_generics(parts: &ModuleParts<'_>) -> (&'static str, &'static str) {
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

    // A guarded dispatch is a thin wrapper that holds the counter around the `match`
    // in `{name}_u`. Entering the counter inside the `match` itself would give each of
    // its (often thousands of) arms a cleanup path and a stack frame, and turn the
    // arms' tail calls into plain calls; the wrapper keeps the `match` frameless.
    let guard_limit = ctx
        .has_call_depth_dispatch_guard(type_index)
        .then_some(ctx.call_depth_limit)
        .flatten();
    let mut lines = Vec::new();
    let body_name = if let Some(limit) = guard_limit {
        let tc_arg = if tc { ", __tc" } else { "" };
        lines.push(format!("pub fn {name}({params}){ret} {{"));
        lines.push(format!("    let __depth = CallDepth::enter({limit});"));
        lines.push(format!("    self.{name}_u(f{sep}{arg_list}{tc_arg})"));
        lines.push("}".to_string());
        format!("{name}_u")
    } else {
        name
    };
    let vis = if guard_limit.is_some() { "" } else { "pub " };
    lines.push(format!("{vis}fn {body_name}({params}){ret} {{"));
    lines.push("    match f {".to_string());
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
pub(super) fn extern_registry_lines(
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
pub(super) fn import_trait_lines(
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
            ctx.table
                .elements
                .get(i)
                .copied()
                .unwrap_or(ValType::FUNCREF),
        );
        lines.push(format!("    fn {get}(&self) -> &[{elem}];"));
        lines.push(format!("    fn {get_mut}(&mut self) -> &mut Vec<{elem}>;"));
    }
    // The tail imports follow the defined functions in the index space.
    let tail_base = imports.len().saturating_add(ctx.funcs.len());
    let tail = ctx
        .tail_imports
        .iter()
        .enumerate()
        .map(|(k, im)| (tail_base.saturating_add(k), im));
    for (j, im) in imports.iter().enumerate().chain(tail) {
        // A recognised WASI import is a native inherent method, not a host
        // trait method; skip it while keeping `import{j}` aligned to the
        // absolute import index used by `call_expr`.
        if !im.is_host() {
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
