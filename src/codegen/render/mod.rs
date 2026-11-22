//! Rendering of a stateful module's `Instance`: the layout shared by every section and the
//! head/tail assembly, with each section's renderer in its own submodule.

mod accessors;
mod ctor;
mod exports;
mod fields;
mod globals;
mod imports;
mod methods;
mod root;
mod statics;
mod threads;

use self::accessors::{memory_accessor_lines, table_accessor_lines};
use self::ctor::{constructor_lines, with_host_lines};
use self::exports::EMSCRIPTEN_STACK_ENTRIES;
pub(super) use self::exports::{entry_point_lines, internal_entry_names};
use self::fields::instance_field_lines;
pub(crate) use self::imports::needs_host_trait;
use self::imports::{
    dispatch_method_block, extern_registry_lines, host_generics, import_trait_lines,
};
use self::methods::{helper_method_lines, wasi_method_lines};
use self::root::shared_memory_runtime_lines;
pub(super) use self::root::{RootDeps, chunk_prelude, render_lib_root};
use self::statics::{segment_statics, trimmed_data};
use super::render_cont::{continuation_method_lines, continuation_runtime_lines};
use super::{ALLOW, DataFiles, Helper, ModuleCtx, ModuleParts, WasiFn, indent};
use crate::TranspileError;
use std::collections::HashSet;

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
    emscripten_stack: bool,
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
        let emscripten_stack = EMSCRIPTEN_STACK_ENTRIES
            .iter()
            .all(|(name, ..)| parts.exports.iter().any(|e| e.name == *name));
        Layout {
            has_imports: needs_host_trait(parts),
            mem_imported,
            table_imported,
            wasi_files,
            wasi_preopen,
            wasi_threads,
            emscripten_stack,
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
/// The body is assembled by combining the per-section renderers in the submodules: the
/// module-scope declarations (host trait, shared-memory runtime, passive
/// statics, continuation runtime) come first, then the `struct` fields, then the
/// `impl`'s inherent methods (constructors, memory/table accessors, WASI/helper/
/// dispatch/continuation methods), and finally each function's own source
/// streamed in.
#[expect(
    clippy::too_many_arguments,
    reason = "forwards render_module_head's inputs plus the function sources"
)]
fn render_module(
    out: &mut String,
    parts: &ModuleParts<'_>,
    ctx: &ModuleCtx<'_>,
    sources: &[&str],
    used: &HashSet<(Helper, u32)>,
    dispatch_sigs: &HashSet<u32>,
    typed_trap_type: bool,
    side: Option<&mut DataFiles>,
) -> Result<(), TranspileError> {
    render_module_head(out, parts, ctx, used, dispatch_sigs, typed_trap_type, side)?;
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
    typed_trap_type: bool,
    side: Option<&mut DataFiles>,
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
            "// (except a `memory.copy` between two memories) are UNCHECKED. An out-of-bounds"
                .to_string(),
        );
        lines.push(
            "// access is undefined behaviour, not a wasm trap. Only sound for modules trusted"
                .to_string(),
        );
        lines.push("// to stay in bounds.".to_string());
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
    if ctx.memory.shared {
        lines.extend(shared_memory_runtime_lines(ctx.typed_traps));
        lines.push(String::new());
    }
    // Module-scope statics backing the data and element segments.
    let trimmed = trimmed_data(parts, ctx, &layout);
    lines.extend(segment_statics(parts.data, &trimmed, parts.elements, side));
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
    inner.extend(constructor_lines(parts, ctx, &layout, &trimmed)?);
    if ctx.reentrant_imports {
        inner.extend(with_host_lines());
    }
    inner.extend(memory_accessor_lines(parts, ctx));
    inner.extend(table_accessor_lines(parts)?);
    inner.extend(wasi_method_lines(
        parts.imports,
        &layout,
        ctx.memory.shared,
        typed_trap_type,
    ));
    inner.extend(helper_method_lines(
        used,
        parts.memories,
        ctx.memory.shared,
        ctx.unsafe_memory,
        ctx.typed_traps,
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
