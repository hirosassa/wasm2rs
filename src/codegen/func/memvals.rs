use wasmparser::{MemArg, ValType};

use super::super::{Helper, Val, helper_name, memarg_offset};
use crate::TranspileError;

impl<'a> super::FuncGen<'a> {
    // ----- locals ----------------------------------------------------------
    pub(super) fn local_store(
        &mut self,
        local_index: u32,
        keep: bool,
    ) -> Result<(), TranspileError> {
        // Fix any operand that reads a mutable local before we overwrite it.
        self.spill_nonstable()?;
        let value = if keep {
            self.stack
                .last()
                .cloned()
                .ok_or(TranspileError::StackUnderflow)?
        } else {
            self.pop()?
        };
        self.line(format!("l{local_index} = {};", value.code));
        Ok(())
    }

    pub(super) fn local_ty(&self, local_index: u32) -> Result<ValType, TranspileError> {
        self.local_types
            .get(local_index as usize)
            .copied()
            .ok_or_else(|| TranspileError::Unsupported("local index out of range".into()))
    }

    // ----- globals ---------------------------------------------------------

    /// Resolve a global index to `(type, mutable, imported)`, spanning imported
    /// globals (the low indices) then defined globals. `imported` is true when
    /// the global is host-backed.
    pub(super) fn global(
        &self,
        global_index: u32,
    ) -> Result<(ValType, bool, bool), TranspileError> {
        let n_imported = self.ctx.imported_globals.len();
        let idx = global_index as usize;
        let (entry, imported) = if idx < n_imported {
            (self.ctx.imported_globals.get(idx), true)
        } else {
            (self.ctx.globals.get(idx - n_imported), false)
        };
        entry
            .map(|&(ty, mutable)| (ty, mutable, imported))
            .ok_or_else(|| TranspileError::Unsupported("global index out of range".into()))
    }

    pub(super) fn global_get(&mut self, global_index: u32) -> Result<(), TranspileError> {
        let (ty, mutable, imported) = self.global(global_index)?;
        // An imported global is fetched from the host and is always unstable (a
        // host getter should not be re-evaluated, so it is materialised when it
        // matters); a defined one is a field, unstable only when mutable.
        let (code, stable) = if imported {
            (format!("self.imports.get_global{global_index}()"), false)
        } else {
            (format!("self.g{global_index}"), !mutable)
        };
        self.push(Val { code, ty, stable });
        Ok(())
    }

    pub(super) fn global_set(&mut self, global_index: u32) -> Result<(), TranspileError> {
        let (_, mutable, imported) = self.global(global_index)?;
        if !mutable {
            return Err(TranspileError::Unsupported(
                "set of immutable global".into(),
            ));
        }
        self.spill_nonstable()?;
        let value = self.pop()?;
        if imported {
            self.line(format!(
                "self.imports.set_global{global_index}({});",
                value.code
            ));
        } else {
            self.line(format!("self.g{global_index} = {};", value.code));
        }
        Ok(())
    }

    // ----- linear memory ---------------------------------------------------

    pub(super) fn require_memory(&self) -> Result<(), TranspileError> {
        if self.ctx.has_memory {
            Ok(())
        } else {
            Err(TranspileError::Unsupported(
                "memory instruction without a memory section".into(),
            ))
        }
    }

    pub(super) fn require_table(&self) -> Result<(), TranspileError> {
        if self.ctx.has_table {
            Ok(())
        } else {
            Err(TranspileError::Unsupported(
                "table instruction without a table section".into(),
            ))
        }
    }

    /// Require that a bulk-memory/table operand references memory/table 0, the
    /// only one supported until multi-memory/multi-table lands.
    pub(super) fn require_zero_index(&self, index: u32, what: &str) -> Result<(), TranspileError> {
        if index == 0 {
            Ok(())
        } else {
            Err(TranspileError::Unsupported(format!(
                "{what} on a non-zero index"
            )))
        }
    }

    pub(super) fn load(
        &mut self,
        helper: Helper,
        ty: ValType,
        memarg: MemArg,
    ) -> Result<(), TranspileError> {
        self.require_memory()?;
        let offset = memarg_offset(memarg)?;
        let addr = self.pop()?;
        self.used_helpers.insert(helper);
        self.push(Val {
            code: format!(
                "self.{}(({}) as u32, {offset}u32)",
                helper_name(helper),
                addr.code
            ),
            ty,
            // The result depends on memory contents, which a store can change.
            stable: false,
        });
        Ok(())
    }

    pub(super) fn store(&mut self, helper: Helper, memarg: MemArg) -> Result<(), TranspileError> {
        self.require_memory()?;
        let offset = memarg_offset(memarg)?;
        // Memory is about to change; fix any operand that reads from it.
        self.spill_nonstable()?;
        let value = self.pop()?;
        let addr = self.pop()?;
        self.used_helpers.insert(helper);
        self.line(format!(
            "self.{}(({}) as u32, {offset}u32, {});",
            helper_name(helper),
            addr.code,
            value.code
        ));
        Ok(())
    }

    pub(super) fn memory_size(&mut self) -> Result<(), TranspileError> {
        self.require_memory()?;
        // Materialise into a temp: `self.mem()` borrows the instance, so leaving
        // it inline could clash with another `self.mem()`/method call in the same
        // enclosing expression (e.g. imported memory, where `mem()` routes
        // through the host).
        let name = self.fresh_temp();
        self.line(format!(
            "let {name}: i32 = (self.mem().len() / 65536) as i32;"
        ));
        self.push(Val {
            code: name,
            ty: ValType::I32,
            stable: true,
        });
        Ok(())
    }

    pub(super) fn memory_grow(&mut self) -> Result<(), TranspileError> {
        self.require_memory()?;
        self.spill_nonstable()?;
        let delta = self.pop()?;
        self.used_helpers.insert(Helper::Grow);
        let name = self.fresh_temp();
        self.line(format!(
            "let {name}: i32 = self.memory_grow({});",
            delta.code
        ));
        self.push(Val {
            code: name,
            ty: ValType::I32,
            stable: true,
        });
        Ok(())
    }

    /// Pop the three `i32` operands (dest, src/value, len) of a bulk operation,
    /// spilling first since the operation mutates memory/table. Returned in
    /// source order: `(dest, mid, len)`.
    pub(super) fn pop_bulk_operands(&mut self) -> Result<(Val, Val, Val), TranspileError> {
        self.spill_nonstable()?;
        let len = self.pop()?;
        let mid = self.pop()?;
        let dest = self.pop()?;
        Ok((dest, mid, len))
    }

    pub(super) fn memory_fill(&mut self, mem: u32) -> Result<(), TranspileError> {
        self.require_memory()?;
        self.require_zero_index(mem, "memory.fill")?;
        let (dest, val, len) = self.pop_bulk_operands()?;
        self.used_helpers.insert(Helper::MemoryFill);
        self.line(format!(
            "self.memory_fill(({}) as u32, {}, ({}) as u32);",
            dest.code, val.code, len.code
        ));
        Ok(())
    }

    pub(super) fn memory_copy(&mut self, dst_mem: u32, src_mem: u32) -> Result<(), TranspileError> {
        self.require_memory()?;
        self.require_zero_index(dst_mem, "memory.copy")?;
        self.require_zero_index(src_mem, "memory.copy")?;
        let (dest, src, len) = self.pop_bulk_operands()?;
        self.used_helpers.insert(Helper::MemoryCopy);
        self.line(format!(
            "self.memory_copy(({}) as u32, ({}) as u32, ({}) as u32);",
            dest.code, src.code, len.code
        ));
        Ok(())
    }
}
