use wasmparser::{AbstractHeapType, HeapType, ValType};

use super::super::helpers::mem_accessor;
use super::super::{Helper, Val};
use crate::TranspileError;

impl<'a> super::FuncGen<'a> {
    pub(super) fn table_copy(
        &mut self,
        dst_table: u32,
        src_table: u32,
    ) -> Result<(), TranspileError> {
        self.require_table()?;
        self.require_zero_index(dst_table, "table.copy")?;
        self.require_zero_index(src_table, "table.copy")?;
        let (dest, src, len) = self.pop_bulk_operands()?;
        self.used_helpers.insert((Helper::TableCopy, 0));
        self.line(format!(
            "self.table_copy(({}) as u32, ({}) as u32, ({}) as u32);",
            dest.code, src.code, len.code
        ));
        Ok(())
    }

    /// `table.get`: push the funcref at the given index. An out-of-bounds index
    /// panics on the slice access (a trap). The value is non-stable because a
    /// later `table.set`/`table.grow` can change it.
    pub(super) fn table_get(&mut self, table: u32) -> Result<(), TranspileError> {
        self.require_table()?;
        self.require_zero_index(table, "table.get")?;
        let index = self.pop()?;
        let element = self.ctx.table_element.unwrap_or(ValType::FUNCREF);
        self.push_combined(
            format!("self.table()[({}) as u32 as usize]", index.code),
            element,
            false,
        )
    }

    /// `table.set`: write a funcref at the given index. Out-of-bounds panics (a
    /// trap). Spilled first since it mutates the table.
    pub(super) fn table_set(&mut self, table: u32) -> Result<(), TranspileError> {
        self.require_table()?;
        self.require_zero_index(table, "table.set")?;
        self.spill_nonstable()?;
        let value = self.pop()?;
        let index = self.pop()?;
        self.line(format!(
            "self.table_mut()[({}) as u32 as usize] = ({}) as u32;",
            index.code, value.code
        ));
        Ok(())
    }

    /// `table.size`: push the current table length. Non-stable because
    /// `table.grow` can change it.
    pub(super) fn table_size(&mut self, table: u32) -> Result<(), TranspileError> {
        self.require_table()?;
        self.require_zero_index(table, "table.size")?;
        self.push(Val {
            code: "(self.table().len() as i32)".to_string(),
            ty: ValType::I32,
            stable: false,
        });
        Ok(())
    }

    /// `table.grow`: append `delta` (an unsigned count) copies of the init
    /// funcref and push the old length. A table is indexed by `u32`, so it holds
    /// at most `u32::MAX` entries; per the spec, a delta that would exceed that
    /// (e.g. a "negative" delta, i.e. a huge unsigned count) pushes -1 rather
    /// than attempting an impossible allocation. Spilled first since it mutates
    /// the table.
    pub(super) fn table_grow(&mut self, table: u32) -> Result<(), TranspileError> {
        self.require_table()?;
        self.require_zero_index(table, "table.grow")?;
        self.spill_nonstable()?;
        let delta = self.pop()?;
        let init = self.pop()?;
        let name = self.fresh_temp();
        self.line(format!(
            "let {name}: i32 = {{ let n = ({}) as u32 as usize; let old = self.table().len(); \
             match old.checked_add(n) {{ \
             Some(len) if len <= u32::MAX as usize => {{ self.table_mut().resize(len, ({}) as u32); old as i32 }} \
             _ => -1, }} }};",
            delta.code, init.code
        ));
        self.push(Val {
            code: name,
            ty: ValType::I32,
            stable: true,
        });
        Ok(())
    }

    /// `table.fill`: write the init funcref into `[dest, dest+len)`. The range is
    /// bounds-checked before any write (a trap on overflow, no partial write).
    pub(super) fn table_fill(&mut self, table: u32) -> Result<(), TranspileError> {
        self.require_table()?;
        self.require_zero_index(table, "table.fill")?;
        let (dest, val, len) = self.pop_bulk_operands()?;
        self.used_helpers.insert((Helper::TableFill, 0));
        self.line(format!(
            "self.table_fill(({}) as u32, ({}) as u32, ({}) as u32);",
            dest.code, val.code, len.code
        ));
        Ok(())
    }

    /// Require that segment `index` (named by `kind`, e.g. "data") is passive;
    /// the init/drop instructions only reference passive segments here (active
    /// ones are auto-copied then implicitly dropped at instantiation).
    pub(super) fn require_passive(
        &self,
        passive: &[bool],
        index: u32,
        kind: &str,
    ) -> Result<(), TranspileError> {
        if passive.get(index as usize).copied().unwrap_or(false) {
            Ok(())
        } else {
            Err(TranspileError::Unsupported(format!(
                "{kind} segment {index} is not passive (init/drop needs a passive segment)"
            )))
        }
    }

    /// Emit a bulk copy of `len` elements from a retained passive segment
    /// (`src_seg`, e.g. `self.data0`) into `dst` (e.g. `self.mem_mut()`). A range
    /// exceeding either side — including a dropped, now-empty segment — panics,
    /// reproducing the wasm trap; the destination is bounds-checked first, so no
    /// partial write occurs. The segment slice is copied into a local first so
    /// that `dst` (which may borrow the whole instance, e.g. `self.mem_mut()` for
    /// imported memory) does not clash with the `&self` segment field borrow.
    pub(super) fn emit_bulk_init(
        &mut self,
        dst: &str,
        src_seg: &str,
        dest: &Val,
        src: &Val,
        len: &Val,
    ) {
        self.line(format!(
            "{{ let seg = {src_seg}; let n = ({}) as usize; let s = ({}) as usize; \
             let d = ({}) as usize; {dst}[d..d + n].copy_from_slice(&seg[s..s + n]); }}",
            len.code, src.code, dest.code
        ));
    }

    pub(super) fn memory_init(&mut self, data_index: u32, mem: u32) -> Result<(), TranspileError> {
        self.require_memory()?;
        let mem = self.require_memory_index(mem)?;
        self.require_passive(&self.ctx.data_passive, data_index, "data")?;
        let (dest, src, len) = self.pop_bulk_operands()?;
        // `memory.init` copies into the memory named by `mem`; memory 0 keeps the
        // `mem_mut()` accessor, higher memories use their `mem{i}_mut()`. A shared
        // memory has no accessor — it locks its bytes for the whole copy instead.
        let dst = if self.ctx.memory_shared {
            "self.memory.bytes()".to_string()
        } else {
            let (_, dst_get_mut) = mem_accessor(mem);
            format!("self.{dst_get_mut}()")
        };
        self.emit_bulk_init(&dst, &format!("self.data{data_index}"), &dest, &src, &len);
        Ok(())
    }

    pub(super) fn data_drop(&mut self, data_index: u32) -> Result<(), TranspileError> {
        self.require_passive(&self.ctx.data_passive, data_index, "data")?;
        self.line(format!("self.data{data_index} = &[];"));
        Ok(())
    }

    pub(super) fn table_init(&mut self, elem_index: u32, table: u32) -> Result<(), TranspileError> {
        self.require_table()?;
        self.require_zero_index(table, "table.init")?;
        self.require_passive(&self.ctx.elem_passive, elem_index, "elem")?;
        let (dest, src, len) = self.pop_bulk_operands()?;
        self.emit_bulk_init(
            "self.table_mut()",
            &format!("self.elem{elem_index}"),
            &dest,
            &src,
            &len,
        );
        Ok(())
    }

    pub(super) fn elem_drop(&mut self, elem_index: u32) -> Result<(), TranspileError> {
        self.require_passive(&self.ctx.elem_passive, elem_index, "elem")?;
        self.line(format!("self.elem{elem_index} = &[];"));
        Ok(())
    }

    /// `ref.null t`: push a null reference. Both `funcref` and `externref` are
    /// represented as a `u32` (`u32::MAX` is null).
    pub(super) fn ref_null(&mut self, hty: HeapType) -> Result<(), TranspileError> {
        let ty = match hty {
            HeapType::Abstract {
                ty: AbstractHeapType::Func,
                ..
            } => ValType::FUNCREF,
            HeapType::Abstract {
                ty: AbstractHeapType::Extern,
                ..
            } => ValType::EXTERNREF,
            _ => {
                return Err(TranspileError::Unsupported(format!(
                    "ref.null of unsupported type {hty:?}"
                )));
            }
        };
        self.push(Val {
            code: "u32::MAX".to_string(),
            ty,
            stable: true,
        });
        Ok(())
    }

    /// `ref.func f`: push the funcref for function `f` (its full index).
    pub(super) fn ref_func(&mut self, function_index: u32) {
        self.push(Val {
            code: format!("{function_index}u32"),
            ty: ValType::FUNCREF,
            stable: true,
        });
    }

    /// `ref.is_null`: pop a funcref and push 1 if it is null, else 0.
    pub(super) fn ref_is_null(&mut self) -> Result<(), TranspileError> {
        let r = self.pop()?;
        self.push_combined(
            format!("i32::from({} == u32::MAX)", r.code),
            ValType::I32,
            r.stable,
        )
    }
}
