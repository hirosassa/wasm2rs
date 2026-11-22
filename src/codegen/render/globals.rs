//! Getter/setter methods for exported numeric globals.

use crate::codegen::{GlobalExportInfo, ModuleCtx, ModuleParts};
use wasmparser::ValType;

/// An exported numeric global resolved against the module's globals.
pub(super) struct NumericGlobal {
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
pub(super) fn global_getter_lines(
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
pub(super) fn settable_globals<'a>(
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
pub(super) fn global_setter_lines(
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
