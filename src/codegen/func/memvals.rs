use wasmparser::{MemArg, MemArgTarget, ValType};

use super::super::helpers::{helper_method_name, index_expr64, mem_accessor};
use super::super::{Helper, Val, WASM_PAGE_SIZE, memarg_offset, rust_type};
use crate::TranspileError;

/// The Rust integer type an atomic load/RMW/cmpxchg result is cast to on the shared
/// path. An atomic access is always over an i32- or i64-typed cell; any other
/// type is unreachable here (validation rejects it), so it falls back to `i64`.
fn shared_result_ty(ty: ValType) -> &'static str {
    match ty {
        ValType::I32 => "i32",
        _ => "i64",
    }
}

impl<'a> super::FuncGen<'a> {
    /// The Rust expression that consumes `v` by value. A managed (`GcRef`)
    /// reference is not `Copy`, so a value read from a place that stays live
    /// (a local, a global field, an operand read twice off the stack) must be
    /// `.clone()`d — a cheap `Rc` bump — rather than moved, or a later read of
    /// the same place would be a use-after-move that fails to compile. Scalars
    /// and the `u32` funcref/externref/contref handles are `Copy` and pass
    /// through unchanged.
    pub(super) fn move_val(&self, v: &Val) -> Result<String, TranspileError> {
        Ok(if rust_type(v.ty, self.ctx.type_kinds)? == "GcRef" {
            format!("({}).clone()", v.code)
        } else {
            v.code.clone()
        })
    }

    // ----- locals ----------------------------------------------------------

    /// `local.get`: push the local's expression. A local that is never mutated
    /// is stable (re-reading `l{i}` always yields the same value); a mutable one
    /// is not, so a later mutation forces it to be spilled first.
    pub(super) fn local_get(&mut self, local_index: u32) -> Result<(), TranspileError> {
        let ty = self.local_ty(local_index)?;
        let stable = !self.mutable_locals.contains(local_index);
        // `local_ty` succeeded, so the index is in range of `local_names` too.
        let code = self
            .local_names
            .get(local_index as usize)
            .cloned()
            .ok_or_else(|| TranspileError::Unsupported("local index out of range".into()))?;
        self.push(Val {
            code,
            ty,
            stable,
            may_trap: false,
        });
        Ok(())
    }

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
        self.line(format!("l{local_index} = {};", self.move_val(&value)?));
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
        self.push(Val {
            code,
            ty,
            stable,
            may_trap: false,
        });
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
        let code = self.move_val(&value)?;
        if imported {
            self.line(format!("self.imports.set_global{global_index}({code});"));
        } else {
            self.line(format!("self.g{global_index} = {code};"));
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

    /// Require that `table` names a declared table, returning it so a caller can
    /// select the table-specific accessor.
    pub(super) fn require_table_index(&self, table: u32) -> Result<u32, TranspileError> {
        self.require_table()?;
        if (table as usize) < self.ctx.table_elements.len() {
            Ok(table)
        } else {
            Err(TranspileError::Unsupported(
                "table instruction with an out-of-range table index".into(),
            ))
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

    /// The linear memory a `MemArg` addresses; GC-array-payload targets (the
    /// array-memory proposal) are not supported.
    pub(super) fn memarg_memory(&self, memarg: MemArg) -> Result<u32, TranspileError> {
        match memarg.target {
            MemArgTarget::Memory(mem) => self.require_memory_index(mem),
            MemArgTarget::Array(_) => Err(TranspileError::Unsupported(
                "memory instruction targeting a GC array payload".into(),
            )),
        }
    }

    /// Whether linear memory `mem` is 64-bit (memory64 proposal).
    pub(super) fn is_memory64(&self, mem: u32) -> bool {
        self.ctx
            .memory64
            .get(mem as usize)
            .copied()
            .unwrap_or(false)
    }

    /// The Rust type of a wasm address into memory `mem`: `i64` for a 64-bit
    /// memory, `i32` otherwise.
    pub(super) fn addr_rust_ty(&self, mem: u32) -> &'static str {
        if self.is_memory64(mem) { "i64" } else { "i32" }
    }

    /// The wasm value type of an address into memory `mem`.
    pub(super) fn addr_val_ty(&self, mem: u32) -> ValType {
        if self.is_memory64(mem) {
            ValType::I64
        } else {
            ValType::I32
        }
    }

    /// The static offset as a `u32`, for the shared-memory atomics (a shared
    /// memory is always 32-bit). A non-shared memory is lowered through the
    /// typed [`Self::offset_lit`] instead, and may legitimately carry a 64-bit
    /// offset, so it must not be forced through `u32` here (the value is `0` and
    /// unused on that path).
    pub(super) fn shared_offset(&self, memarg: MemArg) -> Result<u32, TranspileError> {
        if self.ctx.memory_shared {
            memarg_offset(memarg)
        } else {
            Ok(0)
        }
    }

    /// The static offset of `memarg` as a literal for a memory-access helper.
    /// The helper's `offset` parameter is `u64` for a 64-bit memory and `u32`
    /// otherwise, so the literal needs no type suffix.
    pub(super) fn offset_lit(&self, memarg: MemArg, mem: u32) -> Result<String, TranspileError> {
        if self.is_memory64(mem) {
            Ok(memarg.offset.to_string())
        } else {
            Ok(memarg_offset(memarg)?.to_string())
        }
    }

    pub(super) fn load(
        &mut self,
        helper: Helper,
        ty: ValType,
        memarg: MemArg,
    ) -> Result<(), TranspileError> {
        self.require_memory()?;
        let mem = self.memarg_memory(memarg)?;
        let offset = self.offset_lit(memarg, mem)?;
        let addr = self.pop()?;
        self.used_helpers.insert((helper, mem));
        // The result depends on memory contents, which a store can change, so
        // it is never stable.
        self.push_trapping(
            format!(
                "self.{}({}, {offset})",
                helper_method_name(helper, mem),
                addr.code
            ),
            ty,
        )
    }

    pub(super) fn store(&mut self, helper: Helper, memarg: MemArg) -> Result<(), TranspileError> {
        self.require_memory()?;
        let mem = self.memarg_memory(memarg)?;
        let offset = self.offset_lit(memarg, mem)?;
        // Memory is about to change; fix any operand that reads from it. The
        // address and value are consumed here, so inline them and freeze only
        // the survivors below.
        self.freeze_survivors(2)?;
        let value = self.pop()?;
        let addr = self.pop()?;
        self.used_helpers.insert((helper, mem));
        self.line(format!(
            "self.{}({}, {offset}, {});",
            helper_method_name(helper, mem),
            addr.code,
            value.code
        ));
        Ok(())
    }

    /// The byte width an atomic load/store helper accesses.
    fn atomic_access_width(helper: Helper) -> usize {
        match helper {
            Helper::Load8U | Helper::Load8UI64 | Helper::Store8 | Helper::Store8I64 => 1,
            Helper::Load16U | Helper::Load16UI64 | Helper::Store16 | Helper::Store16I64 => 2,
            Helper::LoadI32 | Helper::Load32UI64 | Helper::StoreI32 | Helper::Store32I64 => 4,
            _ => 8,
        }
    }

    /// An atomic load. On a shared memory it is a real (sequentially consistent)
    /// atomic access that traps when misaligned; otherwise the instance owns its
    /// memory and it is the plain load.
    pub(super) fn atomic_load(
        &mut self,
        helper: Helper,
        ty: ValType,
        memarg: MemArg,
    ) -> Result<(), TranspileError> {
        if !self.ctx.memory_shared {
            return self.load(helper, ty, memarg);
        }
        self.require_memory()?;
        self.memarg_memory(memarg)?;
        let offset = self.shared_offset(memarg)?;
        // A sequentially consistent load is ordered against every earlier
        // access, so evaluate it now rather than leaving it pending (a deferred
        // load could be read after a later one, or after a fence or store).
        self.spill_nonstable()?;
        let addr = self.pop()?;
        let name = self.fresh_temp();
        self.line(format!(
            "let {name} = self.memory.atomic_load(usize::try_from(({}) as u32 as u64 + {offset}u64).unwrap_or(usize::MAX), {}) as {};",
            addr.code,
            Self::atomic_access_width(helper),
            shared_result_ty(ty)
        ));
        self.push(Val {
            code: name,
            ty,
            stable: true,
            may_trap: false,
        });
        Ok(())
    }

    /// An atomic store (see [`Self::atomic_load`]).
    pub(super) fn atomic_store(
        &mut self,
        helper: Helper,
        memarg: MemArg,
    ) -> Result<(), TranspileError> {
        if !self.ctx.memory_shared {
            return self.store(helper, memarg);
        }
        self.require_memory()?;
        self.memarg_memory(memarg)?;
        let offset = self.shared_offset(memarg)?;
        self.freeze_survivors(2)?;
        let value = self.pop()?;
        let addr = self.pop()?;
        self.line(format!(
            "self.memory.atomic_store(usize::try_from(({}) as u32 as u64 + {offset}u64).unwrap_or(usize::MAX), {}, ({}) as u64);",
            addr.code,
            Self::atomic_access_width(helper),
            value.code
        ));
        Ok(())
    }

    /// `atomic.fence`: a sequentially consistent fence on a shared memory; a
    /// no-op when the instance owns its memory.
    pub(super) fn atomic_fence(&mut self) -> Result<(), TranspileError> {
        if self.ctx.memory_shared {
            // Operands still pending on the stack came before the fence.
            self.spill_nonstable()?;
            self.line("std::sync::atomic::fence(std::sync::atomic::Ordering::SeqCst);".to_string());
        }
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
        let mem = self.memarg_memory(memarg)?;
        let offset = self.offset_lit(memarg, mem)?;
        let value = self.pop()?;
        let addr = self.pop()?;
        self.used_helpers.insert((helper, mem));
        // The result reads memory, which a store can change, so it is never stable.
        self.push_trapping(
            format!(
                "self.{}({}, {offset}, {}, {lane})",
                helper_method_name(helper, mem),
                addr.code,
                value.code
            ),
            ValType::V128,
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
        let mem = self.memarg_memory(memarg)?;
        let offset = self.offset_lit(memarg, mem)?;
        // Memory is about to change; fix any operand that reads from it. The
        // address and value are consumed here, so inline them and freeze only
        // the survivors below.
        self.freeze_survivors(2)?;
        let value = self.pop()?;
        let addr = self.pop()?;
        self.used_helpers.insert((helper, mem));
        self.line(format!(
            "self.{}({}, {offset}, {}, {lane});",
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
        let mem = self.memarg_memory(memarg)?;
        let (load, store, ty, _mask) = width.parts();
        let offset = self.shared_offset(memarg)?;
        let off_lit = self.offset_lit(memarg, mem)?;
        let addr_ty = self.addr_rust_ty(mem);
        // Memory is about to change; fix any operand that reads from it.
        self.spill_nonstable()?;
        let value = self.pop()?;
        let addr = self.pop()?;
        if self.ctx.memory_shared {
            // A shared memory does the whole read-modify-write as one atomic
            // compare-exchange loop (`SharedMemory::atomic_rmw`), so it is atomic
            // across threads; the runtime masks the operand and zero-extends the
            // old value. No load/store helper is used on this path.
            let byte = width.byte_width();
            let code = op.op_code();
            let rust_ty = shared_result_ty(ty);
            let old = self.fresh_temp();
            self.line(format!(
                "let {old} = self.memory.atomic_rmw(usize::try_from(({}) as u32 as u64 + {offset}u64).unwrap_or(usize::MAX), {byte}, {code}, ({}) as u64) as {rust_ty};",
                addr.code, value.code
            ));
            self.push(Val {
                code: old,
                ty,
                stable: true,
                may_trap: false,
            });
            return Ok(());
        }
        self.used_helpers.insert((load, mem));
        self.used_helpers.insert((store, mem));
        // `addr` feeds both the load and the store, so bind it once.
        let addr_tmp = self.fresh_temp();
        self.line(format!("let {addr_tmp}: {addr_ty} = {};", addr.code));
        let old = self.fresh_temp();
        self.line(format!(
            "let {old} = self.{}({addr_tmp}, {off_lit});",
            helper_method_name(load, mem)
        ));
        let new = op.combine(&old, &value.code);
        self.line(format!(
            "self.{}({addr_tmp}, {off_lit}, {new});",
            helper_method_name(store, mem)
        ));
        // `old` is a snapshot, so it never changes once bound.
        self.push(Val {
            code: old,
            ty,
            stable: true,
            may_trap: false,
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
        let mem = self.memarg_memory(memarg)?;
        let (load, store, ty, mask) = width.parts();
        let offset = self.shared_offset(memarg)?;
        let off_lit = self.offset_lit(memarg, mem)?;
        let addr_ty = self.addr_rust_ty(mem);
        // Memory may change; fix any operand that reads from it.
        self.spill_nonstable()?;
        let replacement = self.pop()?;
        let expected = self.pop()?;
        let addr = self.pop()?;
        if self.ctx.memory_shared {
            // One atomic compare-exchange compares at the access width (the runtime
            // masks `expected`) and stores `replacement` only on a match,
            // returning the zero-extended old value either way.
            let byte = width.byte_width();
            let rust_ty = shared_result_ty(ty);
            let old = self.fresh_temp();
            self.line(format!(
                "let {old} = self.memory.atomic_cmpxchg(usize::try_from(({}) as u32 as u64 + {offset}u64).unwrap_or(usize::MAX), {byte}, ({}) as u64, ({}) as u64) as {rust_ty};",
                addr.code, expected.code, replacement.code
            ));
            self.push(Val {
                code: old,
                ty,
                stable: true,
                may_trap: false,
            });
            return Ok(());
        }
        self.used_helpers.insert((load, mem));
        self.used_helpers.insert((store, mem));
        let addr_tmp = self.fresh_temp();
        self.line(format!("let {addr_tmp}: {addr_ty} = {};", addr.code));
        let old = self.fresh_temp();
        self.line(format!(
            "let {old} = self.{}({addr_tmp}, {off_lit});",
            helper_method_name(load, mem)
        ));
        let cmp = match mask {
            Some(mask) => format!("{old} == (({}) & {mask})", expected.code),
            None => format!("{old} == ({})", expected.code),
        };
        self.line(format!(
            "if {cmp} {{ self.{}({addr_tmp}, {off_lit}, {}); }}",
            helper_method_name(store, mem),
            replacement.code
        ));
        // `old` is a snapshot, so it never changes once bound.
        self.push(Val {
            code: old,
            ty,
            stable: true,
            may_trap: false,
        });
        Ok(())
    }

    /// Lower `memory.atomic.notify`. It pops (addr, count) and pushes the number
    /// of woken waiters — always 0 on a single instance, which has no waiters.
    pub(super) fn atomic_notify(&mut self, memarg: MemArg) -> Result<(), TranspileError> {
        self.require_memory()?;
        self.memarg_memory(memarg)?;
        let offset = self.shared_offset(memarg)?;
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
                may_trap: false,
            });
            return Ok(());
        }
        self.pop()?; // count
        self.pop()?; // addr
        self.push(Val {
            code: "0i32".to_string(),
            ty: ValType::I32,
            stable: true,
            may_trap: false,
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
        let mem = self.memarg_memory(memarg)?;
        let offset = self.shared_offset(memarg)?;
        let off_lit = self.offset_lit(memarg, mem)?;
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
                may_trap: false,
            });
            return Ok(());
        }
        self.pop()?; // timeout
        let expected = self.pop()?;
        let addr = self.pop()?;
        self.used_helpers.insert((load, mem));
        let name = self.fresh_temp();
        self.line(format!(
            "let {name}: i32 = if self.{}({}, {off_lit}) != ({}) {{ 1 }} \
             else {{ panic!(\"atomic.wait on a single-threaded instance would block forever\") }};",
            helper_method_name(load, mem),
            addr.code,
            expected.code
        ));
        self.push(Val {
            code: name,
            ty: ValType::I32,
            stable: true,
            may_trap: false,
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
        let addr_ty = self.addr_rust_ty(mem);
        if self.ctx.memory_shared {
            // A shared memory has no `mem()` accessor; measure the locked bytes.
            self.line(format!(
                "let {name}: {addr_ty} = (self.memory.bytes().len() / {WASM_PAGE_SIZE}) as {addr_ty};"
            ));
        } else {
            let (get, _) = mem_accessor(mem);
            self.line(format!(
                "let {name}: {addr_ty} = (self.{get}().len() / {WASM_PAGE_SIZE}) as {addr_ty};"
            ));
        }
        self.push(Val {
            code: name,
            ty: self.addr_val_ty(mem),
            stable: true,
            may_trap: false,
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
            "let {name}: {} = self.{}({});",
            self.addr_rust_ty(mem),
            helper_method_name(Helper::Grow, mem),
            delta.code
        ));
        self.push(Val {
            code: name,
            ty: self.addr_val_ty(mem),
            stable: true,
            may_trap: false,
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
        // A 64-bit memory's helper takes its `i64` operands as they are.
        let (dest, len) = if self.is_memory64(mem) {
            (dest.code, len.code)
        } else {
            (
                format!("({}) as u32", dest.code),
                format!("({}) as u32", len.code),
            )
        };
        self.line(format!(
            "self.{}({dest}, {}, {len});",
            helper_method_name(Helper::MemoryFill, mem),
            val.code
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
            // A 64-bit memory's helper takes its `i64` operands as they are.
            let args = if self.is_memory64(dst_mem) {
                format!("{}, {}, {}", dest.code, src.code, len.code)
            } else {
                format!(
                    "({}) as u32, ({}) as u32, ({}) as u32",
                    dest.code, src.code, len.code
                )
            };
            self.line(format!(
                "self.{}({args});",
                helper_method_name(Helper::MemoryCopy, dst_mem)
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
            // Each operand is as wide as its memory's addresses; the length is
            // `i64` only when both memories are 64-bit.
            let index = |code: &str, is64: bool| {
                if is64 {
                    index_expr64(code)
                } else {
                    format!("({code}) as usize")
                }
            };
            let s = index(&src.code, self.is_memory64(src_mem));
            let d = index(&dest.code, self.is_memory64(dst_mem));
            let n = index(
                &len.code,
                self.is_memory64(src_mem) && self.is_memory64(dst_mem),
            );
            self.line(format!(
                "{{ let s = {s}; let d = {d}; let n = {n}; \
                 let seg = self.{src_get}()[s..s + n].to_vec(); \
                 self.{dst_get_mut}()[d..d + n].copy_from_slice(&seg); }}"
            ));
        }
        Ok(())
    }
}
