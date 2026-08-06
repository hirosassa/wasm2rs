use wasmparser::{MemArg, ValType};

use super::super::helpers::{helper_method_name, mem_accessor};
use super::super::{Helper, Val, memarg_offset};
use crate::TranspileError;

/// The Rust integer type an atomic RMW/cmpxchg result is cast to on the shared
/// path. An atomic access is always over an i32- or i64-typed cell; any other
/// type is unreachable here (validation rejects it), so it falls back to `i64`.
fn shared_result_ty(ty: ValType) -> &'static str {
    match ty {
        ValType::I32 => "i32",
        _ => "i64",
    }
}

impl<'a> super::FuncGen<'a> {
    // ----- locals ----------------------------------------------------------
    pub(super) fn local_store(
        &mut self,
        local_index: u32,
        keep: bool,
    ) -> Result<(), TranspileError> {
        // Fix any operand that reads a mutable local before we overwrite it.
        // `local.set` consumes the value here, so inline it and freeze only the
        // survivors below. `local.tee` leaves the value on the stack, so it must
        // itself be frozen (a later use must not observe the overwritten local).
        let value = if keep {
            self.spill_nonstable()?;
            self.stack
                .last()
                .cloned()
                .ok_or(TranspileError::StackUnderflow)?
        } else {
            self.freeze_survivors(1)?;
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
        // The value is consumed here, so inline it; freeze only the survivors
        // below it against this store's effect on the global.
        self.freeze_survivors(1)?;
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

    /// Require that a bulk-table operand references table 0, the only one
    /// supported until multi-table lands. (Memory operands are validated by
    /// [`Self::require_memory_index`] instead, since multi-memory is supported.)
    pub(super) fn require_zero_index(&self, index: u32, what: &str) -> Result<(), TranspileError> {
        if index == 0 {
            Ok(())
        } else {
            Err(TranspileError::Unsupported(format!(
                "{what} on a non-zero index"
            )))
        }
    }

    /// Require that `mem` names a declared linear memory, returning it so a
    /// caller can select the memory-specific accessor/helper. Rejects an
    /// out-of-range index (a validation failure the emitted code cannot express).
    pub(super) fn require_memory_index(&self, mem: u32) -> Result<u32, TranspileError> {
        if (mem as usize) < self.ctx.n_memories {
            Ok(mem)
        } else {
            Err(TranspileError::Unsupported(
                "memory instruction with an out-of-range memory index".into(),
            ))
        }
    }

    pub(super) fn load(
        &mut self,
        helper: Helper,
        ty: ValType,
        memarg: MemArg,
    ) -> Result<(), TranspileError> {
        self.require_memory()?;
        let mem = self.require_memory_index(memarg.memory)?;
        let offset = memarg_offset(memarg)?;
        let addr = self.pop()?;
        self.used_helpers.insert((helper, mem));
        // The result depends on memory contents, which a store can change, so
        // it is never stable.
        self.push_combined(
            format!(
                "self.{}({}, {offset}u32)",
                helper_method_name(helper, mem),
                addr.code
            ),
            ty,
            false,
        )
    }

    pub(super) fn store(&mut self, helper: Helper, memarg: MemArg) -> Result<(), TranspileError> {
        self.require_memory()?;
        let mem = self.require_memory_index(memarg.memory)?;
        let offset = memarg_offset(memarg)?;
        // Memory is about to change; fix any operand that reads from it. The
        // address and value are consumed here, so inline them and freeze only
        // the survivors below.
        self.freeze_survivors(2)?;
        let value = self.pop()?;
        let addr = self.pop()?;
        self.used_helpers.insert((helper, mem));
        self.line(format!(
            "self.{}({}, {offset}u32, {});",
            helper_method_name(helper, mem),
            addr.code,
            value.code
        ));
        Ok(())
    }

    /// `v128.load*_lane`: read one element from memory into lane `lane` of the
    /// v128 on the stack, leaving the other lanes intact. Pops the vector (on
    /// top) then the address, and pushes the updated vector.
    pub(super) fn load_lane(
        &mut self,
        helper: Helper,
        memarg: MemArg,
        lane: u8,
    ) -> Result<(), TranspileError> {
        self.require_memory()?;
        let mem = self.require_memory_index(memarg.memory)?;
        let offset = memarg_offset(memarg)?;
        let value = self.pop()?;
        let addr = self.pop()?;
        self.used_helpers.insert((helper, mem));
        // The result reads memory, which a store can change, so it is never stable.
        self.push_combined(
            format!(
                "self.{}({}, {offset}u32, {}, {lane})",
                helper_method_name(helper, mem),
                addr.code,
                value.code
            ),
            ValType::V128,
            false,
        )
    }

    /// `v128.store*_lane`: write lane `lane` of the v128 on the stack to memory.
    /// Pops the vector (on top) then the address, and pushes nothing.
    pub(super) fn store_lane(
        &mut self,
        helper: Helper,
        memarg: MemArg,
        lane: u8,
    ) -> Result<(), TranspileError> {
        self.require_memory()?;
        let mem = self.require_memory_index(memarg.memory)?;
        let offset = memarg_offset(memarg)?;
        // Memory is about to change; fix any operand that reads from it. The
        // address and value are consumed here, so inline them and freeze only
        // the survivors below.
        self.freeze_survivors(2)?;
        let value = self.pop()?;
        let addr = self.pop()?;
        self.used_helpers.insert((helper, mem));
        self.line(format!(
            "self.{}({}, {offset}u32, {}, {lane});",
            helper_method_name(helper, mem),
            addr.code,
            value.code
        ));
        Ok(())
    }

    /// Lower an atomic read-modify-write. The instance owns its memory, so this
    /// is a plain load of the old value, a `combine` with the operand, and a
    /// store of the result; the (zero-extended) old value is pushed. `load` reads
    /// the access width (zero-extending for narrow widths) and `store` truncates.
    pub(super) fn atomic_rmw(
        &mut self,
        width: super::AtomicWidth,
        op: super::RmwOp,
        memarg: MemArg,
    ) -> Result<(), TranspileError> {
        self.require_memory()?;
        let mem = self.require_memory_index(memarg.memory)?;
        let (load, store, ty, _mask) = width.parts();
        let offset = memarg_offset(memarg)?;
        // Memory is about to change; fix any operand that reads from it.
        self.spill_nonstable()?;
        let value = self.pop()?;
        let addr = self.pop()?;
        if self.ctx.memory_shared {
            // A shared memory does the whole read-modify-write in one critical
            // section (`SharedMemory::atomic_rmw`), so it is atomic across
            // threads; the runtime masks the operand and zero-extends the old
            // value. No load/store helper is used on this path.
            let byte = width.byte_width();
            let code = op.op_code();
            let rust_ty = shared_result_ty(ty);
            let old = self.fresh_temp();
            self.line(format!(
                "let {old} = self.memory.atomic_rmw(({}) as u32 as usize + {offset}usize, {byte}, {code}, ({}) as u64) as {rust_ty};",
                addr.code, value.code
            ));
            self.push(Val {
                code: old,
                ty,
                stable: true,
            });
            return Ok(());
        }
        self.used_helpers.insert((load, mem));
        self.used_helpers.insert((store, mem));
        // `addr` feeds both the load and the store, so bind it once.
        let addr_tmp = self.fresh_temp();
        self.line(format!("let {addr_tmp}: i32 = {};", addr.code));
        let old = self.fresh_temp();
        self.line(format!(
            "let {old} = self.{}({addr_tmp}, {offset}u32);",
            helper_method_name(load, mem)
        ));
        let new = op.combine(&old, &value.code);
        self.line(format!(
            "self.{}({addr_tmp}, {offset}u32, {new});",
            helper_method_name(store, mem)
        ));
        // `old` is a snapshot, so it never changes once bound.
        self.push(Val {
            code: old,
            ty,
            stable: true,
        });
        Ok(())
    }

    /// Lower an atomic compare-exchange. Pops (addr, expected, replacement),
    /// stores `replacement` only when the loaded value equals `expected`, and
    /// pushes the (zero-extended) old value. For a narrow width, `mask` is the
    /// width's low-bit mask so the comparison ignores the operand's high bits
    /// (the spec compares at the access width); `None` compares the full width.
    pub(super) fn atomic_cmpxchg(
        &mut self,
        width: super::AtomicWidth,
        memarg: MemArg,
    ) -> Result<(), TranspileError> {
        self.require_memory()?;
        let mem = self.require_memory_index(memarg.memory)?;
        let (load, store, ty, mask) = width.parts();
        let offset = memarg_offset(memarg)?;
        // Memory may change; fix any operand that reads from it.
        self.spill_nonstable()?;
        let replacement = self.pop()?;
        let expected = self.pop()?;
        let addr = self.pop()?;
        if self.ctx.memory_shared {
            // One critical section compares at the access width (the runtime
            // masks `expected`) and stores `replacement` only on a match,
            // returning the zero-extended old value either way.
            let byte = width.byte_width();
            let rust_ty = shared_result_ty(ty);
            let old = self.fresh_temp();
            self.line(format!(
                "let {old} = self.memory.atomic_cmpxchg(({}) as u32 as usize + {offset}usize, {byte}, ({}) as u64, ({}) as u64) as {rust_ty};",
                addr.code, expected.code, replacement.code
            ));
            self.push(Val {
                code: old,
                ty,
                stable: true,
            });
            return Ok(());
        }
        self.used_helpers.insert((load, mem));
        self.used_helpers.insert((store, mem));
        let addr_tmp = self.fresh_temp();
        self.line(format!("let {addr_tmp}: i32 = {};", addr.code));
        let old = self.fresh_temp();
        self.line(format!(
            "let {old} = self.{}({addr_tmp}, {offset}u32);",
            helper_method_name(load, mem)
        ));
        let cmp = match mask {
            Some(mask) => format!("{old} == (({}) & {mask})", expected.code),
            None => format!("{old} == ({})", expected.code),
        };
        self.line(format!(
            "if {cmp} {{ self.{}({addr_tmp}, {offset}u32, {}); }}",
            helper_method_name(store, mem),
            replacement.code
        ));
        // `old` is a snapshot, so it never changes once bound.
        self.push(Val {
            code: old,
            ty,
            stable: true,
        });
        Ok(())
    }

    /// Lower `memory.atomic.notify`. It pops (addr, count) and pushes the number
    /// of woken waiters — always 0 on a single instance, which has no waiters.
    pub(super) fn atomic_notify(&mut self, memarg: MemArg) -> Result<(), TranspileError> {
        self.require_memory()?;
        self.require_memory_index(memarg.memory)?;
        let offset = memarg_offset(memarg)?;
        if self.ctx.memory_shared {
            // Wake up to `count` waiters parked on the effective byte address
            // (base + static memarg offset), matching the address a waiter
            // registers under. The count/addr are consumed, so freeze only
            // survivors.
            self.spill_nonstable()?;
            let count = self.pop()?;
            let addr = self.pop()?;
            let name = self.fresh_temp();
            self.line(format!(
                "let {name}: i32 = self.memory.notify(({}) as u32 + {offset}u32, ({}) as u32) as i32;",
                addr.code, count.code
            ));
            self.push(Val {
                code: name,
                ty: ValType::I32,
                stable: true,
            });
            return Ok(());
        }
        self.pop()?; // count
        self.pop()?; // addr
        self.push(Val {
            code: "0i32".to_string(),
            ty: ValType::I32,
            stable: true,
        });
        Ok(())
    }

    /// Lower `memory.atomic.wait{32,64}`. It pops (addr, expected, timeout) and
    /// pushes an i32 result. On a single instance nobody can ever notify, so a
    /// matching wait would block forever and instead traps; a non-matching wait
    /// returns 1 ("not equal") immediately, as the spec requires.
    pub(super) fn atomic_wait(
        &mut self,
        load: Helper,
        memarg: MemArg,
    ) -> Result<(), TranspileError> {
        self.require_memory()?;
        let mem = self.require_memory_index(memarg.memory)?;
        let offset = memarg_offset(memarg)?;
        if self.ctx.memory_shared {
            // A real blocking wait: park until notified or the timeout elapses.
            // `wait32` compares a 4-byte cell, `wait64` an 8-byte one. Returns 0
            // (woken), 1 (value mismatch — did not block), or 2 (timed out). The
            // three operands are consumed, so freeze only survivors.
            let byte = if matches!(load, Helper::LoadI64) {
                8
            } else {
                4
            };
            self.spill_nonstable()?;
            let timeout = self.pop()?;
            let expected = self.pop()?;
            let addr = self.pop()?;
            let name = self.fresh_temp();
            self.line(format!(
                "let {name}: i32 = self.memory.wait(({}) as u32 + {offset}u32, ({}) as u64, {byte}, ({}) as i64);",
                addr.code, expected.code, timeout.code
            ));
            self.push(Val {
                code: name,
                ty: ValType::I32,
                stable: true,
            });
            return Ok(());
        }
        self.pop()?; // timeout
        let expected = self.pop()?;
        let addr = self.pop()?;
        self.used_helpers.insert((load, mem));
        let name = self.fresh_temp();
        self.line(format!(
            "let {name}: i32 = if self.{}({}, {offset}u32) != ({}) {{ 1 }} \
             else {{ panic!(\"atomic.wait on a single-threaded instance would block forever\") }};",
            helper_method_name(load, mem),
            addr.code,
            expected.code
        ));
        self.push(Val {
            code: name,
            ty: ValType::I32,
            stable: true,
        });
        Ok(())
    }

    pub(super) fn memory_size(&mut self, mem: u32) -> Result<(), TranspileError> {
        self.require_memory()?;
        let mem = self.require_memory_index(mem)?;
        // Materialise into a temp: `self.mem()` borrows the instance, so leaving
        // it inline could clash with another `self.mem()`/method call in the same
        // enclosing expression (e.g. imported memory, where `mem()` routes
        // through the host).
        let name = self.fresh_temp();
        if self.ctx.memory_shared {
            // A shared memory has no `mem()` accessor; measure the locked bytes.
            self.line(format!(
                "let {name}: i32 = (self.memory.bytes().len() / 65536) as i32;"
            ));
        } else {
            let (get, _) = mem_accessor(mem);
            self.line(format!(
                "let {name}: i32 = (self.{get}().len() / 65536) as i32;"
            ));
        }
        self.push(Val {
            code: name,
            ty: ValType::I32,
            stable: true,
        });
        Ok(())
    }

    pub(super) fn memory_grow(&mut self, mem: u32) -> Result<(), TranspileError> {
        self.require_memory()?;
        let mem = self.require_memory_index(mem)?;
        self.spill_nonstable()?;
        let delta = self.pop()?;
        self.used_helpers.insert((Helper::Grow, mem));
        let name = self.fresh_temp();
        self.line(format!(
            "let {name}: i32 = self.{}({});",
            helper_method_name(Helper::Grow, mem),
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
        let mem = self.require_memory_index(mem)?;
        let (dest, val, len) = self.pop_bulk_operands()?;
        self.used_helpers.insert((Helper::MemoryFill, mem));
        self.line(format!(
            "self.{}(({}) as u32, {}, ({}) as u32);",
            helper_method_name(Helper::MemoryFill, mem),
            dest.code,
            val.code,
            len.code
        ));
        Ok(())
    }

    pub(super) fn memory_copy(&mut self, dst_mem: u32, src_mem: u32) -> Result<(), TranspileError> {
        self.require_memory()?;
        let dst_mem = self.require_memory_index(dst_mem)?;
        let src_mem = self.require_memory_index(src_mem)?;
        let (dest, src, len) = self.pop_bulk_operands()?;
        if dst_mem == src_mem {
            // Same-memory copy: the `memory_copy` helper is a `copy_within`
            // memmove, so overlapping ranges copy correctly. Memory 0 keeps the
            // historic `memory_copy` name and body.
            self.used_helpers.insert((Helper::MemoryCopy, dst_mem));
            self.line(format!(
                "self.{}(({}) as u32, ({}) as u32, ({}) as u32);",
                helper_method_name(Helper::MemoryCopy, dst_mem),
                dest.code,
                src.code,
                len.code
            ));
        } else {
            // Cross-memory copy: the source and destination are distinct fields,
            // so `mem_mut()` (which borrows all of `self`) cannot be held while
            // also reading `mem()`. Read the source range into a temporary
            // `Vec`, then write it into the destination; both slice accesses are
            // bounds-checked, so an out-of-range range panics (a wasm trap). The
            // destination is copied only after the read completes, so a bad
            // source range traps before any write. No helper method is needed.
            let (src_get, _) = mem_accessor(src_mem);
            let (_, dst_get_mut) = mem_accessor(dst_mem);
            self.line(format!(
                "{{ let s = ({}) as usize; let d = ({}) as usize; let n = ({}) as usize; \
                 let seg = self.{src_get}()[s..s + n].to_vec(); \
                 self.{dst_get_mut}()[d..d + n].copy_from_slice(&seg); }}",
                src.code, dest.code, len.code
            ));
        }
        Ok(())
    }
}
