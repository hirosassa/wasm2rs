//! The crate root (`lib.rs`) of a split render, the per-chunk prelude and shared runtime snippets.

use super::exports::entry_point_lines;
use super::imports::host_generics;
use super::render_module;
use crate::TranspileError;
use crate::codegen::runtime::{defines_trap_type, render_rt_helpers};
use crate::codegen::{ALLOW, DataFiles, Helper, ModuleCtx, ModuleParts};
use std::collections::HashSet;

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
pub(super) fn shared_memory_runtime_lines(typed: bool) -> Vec<String> {
    let lines = crate::codegen::runtime::source_lines(include_str!("../rt_src/shared_memory.rs"));
    if typed {
        typed_shared_memory_lines(lines)
    } else {
        lines
    }
}

/// Under `typed_traps`, each `panic!(..)` of the shared-memory runtime (a misaligned
/// atomic, or an out-of-range access) becomes a call to its cold typed trap. A bounds
/// failure of an atomic access is the same trap as one of a plain access, so both
/// use `trap_memory_oob`.
fn typed_shared_memory_lines(lines: Vec<String>) -> Vec<String> {
    let mut out = Vec::with_capacity(lines.len());
    // (indent, text so far) of a `panic!(..)` that rustfmt may have split over lines.
    let mut panic_stmt: Option<(String, String)> = None;
    for line in lines {
        let code = line.trim_start();
        if panic_stmt.is_none() && code.starts_with("panic!(") {
            let indent = line[..line.len() - code.len()].to_string();
            panic_stmt = Some((indent, String::new()));
        }
        match panic_stmt.as_mut() {
            Some((indent, text)) => {
                text.push_str(&line);
                if line.ends_with(");") {
                    out.push(typed_trap_call(indent, text));
                    panic_stmt = None;
                }
            }
            None => out.push(line),
        }
    }
    out
}

fn typed_trap_call(indent: &str, panic_stmt: &str) -> String {
    let trap = if panic_stmt.contains("unaligned atomic access") {
        "trap_unaligned_atomic_access"
    } else {
        "trap_memory_oob"
    };
    format!("{indent}{trap}();")
}

/// Render the *prelude* of one chunk file of a multi-file module: the
/// `use super::*;` that pulls the root's items (the `Instance` type, the
/// `Imports` trait, the runtime helpers and the other chunks' re-exported
/// functions) into scope, and — for a stateful module — the opening of the
/// `impl Instance` block the chunk's methods live in.
///
/// The functions themselves are streamed in after this prelude by
/// [`generate_function_into`](crate::codegen::generate_function_into): a stateless
/// module's are free `pub fn`s emitted directly, while a stateful module's are
/// `&mut self` methods indented one level inside the `impl` (Rust allows a
/// type's inherent impl to be split across files in the same crate), whose
/// closing `}` the caller appends. The private helper methods the bodies call
/// live on the root's impl and are visible here because a chunk module is a
/// descendant of the crate root that defines them.
pub(in crate::codegen) fn chunk_prelude(parts: &ModuleParts<'_>, stateful: bool) -> String {
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
pub(in crate::codegen) struct RootDeps<'a> {
    pub(in crate::codegen) helpers: &'a HashSet<(Helper, u32)>,
    pub(in crate::codegen) rt: &'a HashSet<crate::codegen::Rt>,
    pub(in crate::codegen) simd: &'a HashSet<&'static str>,
    pub(in crate::codegen) dispatch_sigs: &'a HashSet<u32>,
}

pub(in crate::codegen) fn render_lib_root(
    parts: &ModuleParts<'_>,
    ctx: &ModuleCtx<'_>,
    stateful: bool,
    deps: &RootDeps<'_>,
    n_chunks: usize,
    side: Option<&mut DataFiles>,
) -> Result<String, TranspileError> {
    let mut out = String::new();
    let rt_helpers = render_rt_helpers(deps.rt, ctx.typed_traps);
    if !rt_helpers.is_empty() {
        out.push_str(&rt_helpers);
        out.push('\n');
    }
    let simd_helpers = crate::codegen::render_simd_helpers(deps.simd, ctx.simd_intrinsics);
    if !simd_helpers.is_empty() {
        out.push_str(&simd_helpers);
        out.push('\n');
    }

    if stateful {
        // The header impl carries no function bodies; those are added by the
        // chunk files' own `impl Instance` blocks.
        render_module(
            &mut out,
            parts,
            ctx,
            &[],
            deps.helpers,
            deps.dispatch_sigs,
            defines_trap_type(deps.rt, ctx.typed_traps),
            side,
        )?;
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
