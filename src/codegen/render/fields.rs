//! The `pub struct Instance { … }` field list and the `wasi_fds` table it may carry.

use super::Layout;
use super::imports::host_generics;
use crate::TranspileError;
use crate::codegen::helpers::table_accessor;
use crate::codegen::{ModuleCtx, ModuleParts, index_u32, rust_type, table_elem_rust};

/// The descriptor table behind `wasi_fds`: slot `N - 4` holds descriptor `N`,
/// and `free` lists the closed slots below the high-water mark so `path_open`
/// takes the lowest free descriptor (the POSIX rule) without scanning. It
/// derefs to the slot `Vec` for lookups; only `alloc` and `release` change
/// which slots are free.
const WASI_FDS_DEF: &str = "\
#[allow(dead_code)]
#[derive(Default)]
struct WasiFds {
    slots: Vec<Option<(std::fs::File, std::path::PathBuf)>>,
    free: std::collections::BTreeSet<usize>,
}
#[allow(dead_code)]
impl WasiFds {
    fn alloc(&mut self, entry: (std::fs::File, std::path::PathBuf)) -> usize {
        match self.free.pop_first() {
            Some(i) => { self.slots[i] = Some(entry); i }
            None => { self.slots.push(Some(entry)); self.slots.len() - 1 }
        }
    }
    fn release(&mut self, i: usize) {
        if self.slots.get_mut(i).and_then(Option::take).is_some() {
            self.free.insert(i);
        }
    }
}
impl std::ops::Deref for WasiFds {
    type Target = Vec<Option<(std::fs::File, std::path::PathBuf)>>;
    fn deref(&self) -> &Self::Target { &self.slots }
}
impl std::ops::DerefMut for WasiFds {
    fn deref_mut(&mut self) -> &mut Self::Target { &mut self.slots }
}";

/// The `pub struct Instance { … }` declaration: the host handle (when needed),
/// each defined memory buffer (or the shared handle), the table, the globals,
/// the WASI fd table, the retained passive-segment slices, the externref box and
/// the continuation handle table — whichever the module actually needs.
pub(super) fn instance_field_lines(
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

    let mut lines: Vec<String> = Vec::new();
    if layout.wasi_files {
        lines.extend(WASI_FDS_DEF.lines().map(str::to_string));
    }
    lines.push("#[allow(dead_code)]".to_string());
    lines.push(format!("pub struct Instance{decl_generics} {{"));
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
    if ctx.memory.shared {
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
        lines.push(if layout.wasi_threads {
            "    wasi_fds: std::sync::Arc<std::sync::Mutex<WasiFds>>,".to_string()
        } else {
            "    wasi_fds: WasiFds,".to_string()
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
