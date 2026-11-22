//! The `memory`/`table` accessor methods of `impl Instance`.

use crate::TranspileError;
use crate::codegen::helpers::table_accessor;
use crate::codegen::{ModuleCtx, ModuleParts, index_u32, table_elem_rust};

/// The uniform memory accessors (`mem`/`mem_mut`/`memory` for memory 0, then
/// `mem{i}`/`mem{i}_mut`/`memory{i}`), so the load/store/bulk helpers read the
/// same regardless of whether a memory is defined (a field) or imported (lent by
/// the host). A shared module emits none of these — its helpers use
/// `self.memory.bytes()` directly — so the accessor set is empty there.
pub(super) fn memory_accessor_lines(parts: &ModuleParts<'_>, ctx: &ModuleCtx<'_>) -> Vec<String> {
    let mut inner: Vec<String> = Vec::new();
    for (i, m) in parts
        .memories
        .iter()
        .enumerate()
        .filter(|_| !ctx.memory.shared)
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
pub(super) fn table_accessor_lines(parts: &ModuleParts<'_>) -> Result<Vec<String>, TranspileError> {
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
