//! Segment statics and the zero-edge trimming of active data segments.

use super::Layout;
use crate::codegen::{
    DATA_FILE_MIN_BYTES, DataFiles, DataSegment, ElemSegment, ModuleCtx, ModuleParts, SegOffset,
    WASM_PAGE_SIZE, data_include_line, data_static_line,
};
use std::collections::HashSet;

/// The part of an active data segment that is actually copied: `bytes` is the
/// segment without its leading and trailing zeros, `start` its offset into the
/// segment. An empty `bytes` means nothing needs copying at all.
pub(super) struct TrimmedData<'a> {
    pub(super) start: usize,
    pub(super) bytes: &'a [u8],
}

/// Which active data segments can skip their zero edges, indexed like
/// `parts.data` (`None` = copy the whole segment).
///
/// A segment's zeros are only unobservable when they land on the fresh,
/// zero-filled buffer of a memory the module creates, so a host-owned
/// (imported or shared) memory is never trimmed. The segment must also lie
/// fully inside that buffer (a segment that overruns it has to keep trapping at
/// instantiation, however many of its bytes are zero), have a constant offset,
/// and no earlier segment of the same memory may overlap it or have an unknown
/// offset (a later zero must still overwrite an earlier byte).
pub(super) fn trimmed_data<'a>(
    parts: &ModuleParts<'a>,
    ctx: &ModuleCtx<'_>,
    layout: &Layout,
) -> Vec<Option<TrimmedData<'a>>> {
    let mut written: Vec<(u32, u64, u64)> = Vec::new();
    let mut unknown: HashSet<u32> = HashSet::new();
    let host_owned = layout.mem_imported || ctx.memory.shared;
    parts
        .data
        .iter()
        .map(|seg| {
            let base = match seg.offset.as_ref()? {
                SegOffset::Const(value) => *value,
                SegOffset::Runtime { .. } => {
                    unknown.insert(seg.mem_index);
                    return None;
                }
            };
            let len = u64::try_from(seg.bytes.len()).ok()?;
            let end = base.checked_add(len)?;
            let overlaps = written
                .iter()
                .any(|&(m, s, e)| m == seg.mem_index && s < end && base < e);
            written.push((seg.mem_index, base, end));
            let mem = parts.memories.get(usize::try_from(seg.mem_index).ok()?)?;
            let buffer = mem.min_pages.checked_mul(WASM_PAGE_SIZE)?;
            if host_owned
                || mem.imported
                || mem.shared
                || overlaps
                || unknown.contains(&seg.mem_index)
                || end > buffer
            {
                return None;
            }
            let start = seg.bytes.iter().position(|&b| b != 0).unwrap_or(0);
            let stop = seg
                .bytes
                .iter()
                .rposition(|&b| b != 0)
                .map_or(start, |i| i.saturating_add(1));
            let bytes = seg.bytes.get(start..stop)?;
            Some(TrimmedData { start, bytes })
        })
        .collect()
}

/// Module-scope `static`s backing the data and element segments. Every data
/// segment's bytes live in `DATA{d}`: an active one is copied from it when the
/// instance is built (only its non-zero core, see [`trimmed_data`], and nothing
/// at all when that is empty), a passive one is kept as a `&'static` slice of it
/// so `memory.init` can copy from it on demand. Passive element segments
/// likewise back `table.init`.
pub(super) fn segment_statics(
    data: &[DataSegment<'_>],
    trimmed: &[Option<TrimmedData<'_>>],
    elements: &[ElemSegment],
    mut side: Option<&mut DataFiles>,
) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for (d, seg) in data.iter().enumerate() {
        let bytes = match trimmed.get(d).and_then(Option::as_ref) {
            Some(t) if t.bytes.is_empty() => continue,
            Some(t) => t.bytes,
            None => seg.bytes,
        };
        // A large segment's bytes go to a file `rustc` reads raw, instead of a
        // string literal it has to lex; `DATA{d}` keeps its type either way.
        match side.as_deref_mut() {
            Some(files) if bytes.len() >= DATA_FILE_MIN_BYTES => {
                let name = format!("data_{d}.bin");
                lines.push(data_include_line(d, &name, bytes.len()));
                files.push((name, bytes.to_vec()));
            }
            _ => lines.push(data_static_line(d, bytes)),
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
    lines
}
