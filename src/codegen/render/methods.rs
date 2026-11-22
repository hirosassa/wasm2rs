//! The WASI and helper methods of `impl Instance`.

use super::Layout;
use super::threads::{pthread_create_js_lines, thread_spawn_lines};
use crate::codegen::helpers::{HELPER_ORDER, helper_lines, shared_helper_lines};
use crate::codegen::wasi::{WasiAccess, confine_lines, guest_range_lines, iov_loop_lines};
use crate::codegen::{Helper, ImportInfo, WasiFn};
use std::collections::HashSet;

/// The native WASI functions as inherent methods backed by `self.mem()`, each
/// recognised kind emitted once in first-import order.
pub(super) fn wasi_method_lines(
    imports: &[ImportInfo],
    layout: &Layout,
    shared: bool,
    typed_trap_type: bool,
) -> Vec<String> {
    let wasi_files = layout.wasi_files;
    let access = WasiAccess {
        shared_memory: shared,
        locked_fds: layout.wasi_threads && wasi_files,
    };
    let mut inner: Vec<String> = Vec::new();
    let mut wasi_emitted: Vec<WasiFn> = Vec::new();
    if imports
        .iter()
        .any(|im| im.wasi.is_some_and(WasiFn::confines_path))
    {
        inner.extend(confine_lines());
    }
    if imports.iter().any(|im| im.wasi.is_some()) {
        inner.extend(guest_range_lines());
    }
    if imports
        .iter()
        .any(|im| im.wasi.is_some_and(WasiFn::uses_iov_loops))
    {
        inner.push(String::new());
        inner.extend(iov_loop_lines(wasi_files, access));
    }
    for im in imports {
        if let Some(w) = im.wasi
            && !wasi_emitted.contains(&w)
        {
            wasi_emitted.push(w);
            inner.push(String::new());
            // `thread_spawn_lines` clones the `Arc` itself rather than locking it.
            let body = if w == WasiFn::ThreadSpawn {
                thread_spawn_lines(layout, typed_trap_type)
            } else if w == WasiFn::PthreadCreateJs {
                pthread_create_js_lines(layout, typed_trap_type)
            } else {
                w.lines(wasi_files, access)
            };
            inner.extend(body);
        }
    }
    inner
}

/// The used memory-access helper methods, grouped by memory index (0 first, so
/// the historic single-memory helpers keep their position) then in the canonical
/// `HELPER_ORDER` within each memory. A shared module (single memory 0) emits
/// the variants that lock `self.memory.bytes()` instead of borrowing.
pub(super) fn helper_method_lines(
    used: &HashSet<(Helper, u32)>,
    memories: &[crate::codegen::MemInfo],
    memory_shared: bool,
    unsafe_memory: bool,
    typed_traps: bool,
) -> Vec<String> {
    let mut inner: Vec<String> = Vec::new();
    let mut mem_indices: Vec<u32> = used.iter().map(|(_, mem)| *mem).collect();
    mem_indices.sort_unstable();
    mem_indices.dedup();
    for mem in mem_indices {
        for helper in HELPER_ORDER {
            if used.contains(&(helper, mem)) {
                inner.push(String::new());
                // Table helpers touch no memory, so a shared module renders them
                // like any other (needed to type their traps); without
                // `typed_traps` they keep the shared path, unchanged.
                let table_helper = matches!(helper, Helper::TableCopy | Helper::TableFill);
                if memory_shared && !(typed_traps && table_helper) {
                    // The shared-memory backend reads `self.memory.bytes()` per
                    // body; unchecked access is not wired through it, so
                    // `unsafe_memory` does not apply to a shared memory.
                    inner.extend(shared_helper_lines(helper));
                } else {
                    // `memories` is the same index-ordered sequence `ctx.memory.is64`
                    // is derived from; each memory's `memory.grow` is bounded by
                    // its own maximum.
                    let info = memories.get(mem as usize);
                    let is64 = info.is_some_and(|m| m.memory64);
                    let max_pages = info.and_then(|m| m.max_pages);
                    inner.extend(helper_lines(
                        helper,
                        mem,
                        max_pages,
                        unsafe_memory,
                        typed_traps,
                        is64,
                    ));
                }
            }
        }
    }
    inner
}
