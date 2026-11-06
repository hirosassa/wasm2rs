use wasmparser::{AbstractHeapType, HeapType, PackedIndex, RefType, ValType};

use super::super::helpers::{mem_accessor, table_accessor};
use super::super::{CompositeKind, Helper, Val, is_bottom_u32_heap, is_cont_heap};
use crate::TranspileError;

impl<'a> super::FuncGen<'a> {
    pub(super) fn is_table64(&self, table: u32) -> bool {
        self.ctx
            .table64
            .get(table as usize)
            .copied()
            .unwrap_or(false)
    }

    /// The operand `code` as a slot index (`usize`). A 32-bit table's `i32` is
    /// read as unsigned; a 64-bit table's `i64` is converted whole, so an index
    /// above `u32::MAX` stays out of range (a trap) instead of wrapping.
    pub(super) fn table_index_expr(&self, table: u32, code: &str) -> String {
        if self.is_table64(table) {
            format!("usize::try_from(({code}) as u64).unwrap_or(usize::MAX)")
        } else {
            format!("({code}) as u32 as usize")
        }
    }

    /// The wasm value type of a table's indices, sizes and lengths.
    fn table_index_ty(&self, table: u32) -> ValType {
        if self.is_table64(table) {
            ValType::I64
        } else {
            ValType::I32
        }
    }

    pub(super) fn table_copy(
        &mut self,
        dst_table: u32,
        src_table: u32,
    ) -> Result<(), TranspileError> {
        let dst_table = self.require_table_index(dst_table)?;
        let src_table = self.require_table_index(src_table)?;
        let (dest, src, len) = self.pop_bulk_operands()?;
        let any64 = self.is_table64(dst_table) || self.is_table64(src_table);
        if dst_table == 0 && src_table == 0 && !any64 {
            self.used_helpers.insert((Helper::TableCopy, 0));
            self.line(format!(
                "self.table_copy(({}) as u32, ({}) as u32, ({}) as u32);",
                dest.code, src.code, len.code
            ));
            return Ok(());
        }
        // Any other table: inline. Both ranges are bounds-checked before the write
        // (a trap, no partial write). Across tables the source is copied out first
        // (the two accessors each borrow all of `self`); within one table the
        // ranges may overlap, so `copy_within` is used.
        let (_, dst_mut) = table_accessor(dst_table);
        let (src_get, _) = table_accessor(src_table);
        // The length is an `i64` only when both tables are 64-bit.
        let len_expr = if self.is_table64(dst_table) && self.is_table64(src_table) {
            format!(
                "usize::try_from(({}) as u64).unwrap_or(usize::MAX)",
                len.code
            )
        } else {
            format!("({}) as u32 as usize", len.code)
        };
        let src_expr = self.table_index_expr(src_table, &src.code);
        let dest_expr = self.table_index_expr(dst_table, &dest.code);
        let body = if dst_table == src_table {
            format!("self.{dst_mut}().copy_within(s..s + n, d);")
        } else {
            format!(
                "let tmp: Vec<u32> = self.{src_get}()[s..s + n].to_vec(); \
                 self.{dst_mut}()[d..d + n].copy_from_slice(&tmp);"
            )
        };
        self.line(format!(
            "{{ let n = {len_expr}; let s = {src_expr}; let d = {dest_expr}; {body} }}"
        ));
        Ok(())
    }

    /// `table.get`: push the funcref at the given index. An out-of-bounds index
    /// panics on the slice access (a trap). The value is non-stable because a
    /// later `table.set`/`table.grow` can change it.
    pub(super) fn table_get(&mut self, table: u32) -> Result<(), TranspileError> {
        let table = self.require_table_index(table)?;
        let index = self.pop()?;
        let element = self
            .ctx
            .table_elements
            .get(table as usize)
            .copied()
            .unwrap_or(ValType::FUNCREF);
        let (get, _) = table_accessor(table);
        self.push_trapping(
            format!(
                "self.{get}()[{}]",
                self.table_index_expr(table, &index.code)
            ),
            element,
        )
    }

    /// `table.set`: write a funcref at the given index. Out-of-bounds panics (a
    /// trap). Spilled first since it mutates the table.
    pub(super) fn table_set(&mut self, table: u32) -> Result<(), TranspileError> {
        let table = self.require_table_index(table)?;
        self.spill_nonstable()?;
        let value = self.pop()?;
        let index = self.pop()?;
        let (_, get_mut) = table_accessor(table);
        self.line(format!(
            "self.{get_mut}()[{}] = ({}) as u32;",
            self.table_index_expr(table, &index.code),
            value.code
        ));
        Ok(())
    }

    /// `table.size`: push the current table length. Non-stable because
    /// `table.grow` can change it.
    pub(super) fn table_size(&mut self, table: u32) -> Result<(), TranspileError> {
        let table = self.require_table_index(table)?;
        let (get, _) = table_accessor(table);
        let (ty, rust_ty) = if self.is_table64(table) {
            (ValType::I64, "i64")
        } else {
            (ValType::I32, "i32")
        };
        self.push(Val {
            code: format!("(self.{get}().len() as {rust_ty})"),
            ty,
            stable: false,
            may_trap: false,
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
        let table = self.require_table_index(table)?;
        let (get, get_mut) = table_accessor(table);
        self.spill_nonstable()?;
        let delta = self.pop()?;
        let init = self.pop()?;
        let name = self.fresh_temp();
        let ty = self.table_index_ty(table);
        let rust_ty = if self.is_table64(table) { "i64" } else { "i32" };
        // The declared maximum (when any) caps the new length like the `u32::MAX`
        // entry limit does.
        let limit = self
            .ctx
            .table_max
            .get(table as usize)
            .copied()
            .flatten()
            .unwrap_or(u32::MAX);
        // A 64-bit table is still capped at `u32::MAX` entries (an
        // implementation limit), so a larger delta fails with -1.
        self.line(format!(
            "let {name}: {rust_ty} = {{ let n = {}; let old = self.{get}().len(); \
             match old.checked_add(n) {{ \
             Some(len) if len <= {limit}usize => {{ self.{get_mut}().resize(len, ({}) as u32); old as {rust_ty} }} \
             _ => -1, }} }};",
            self.table_index_expr(table, &delta.code),
            init.code
        ));
        self.push(Val {
            code: name,
            ty,
            stable: true,
            may_trap: false,
        });
        Ok(())
    }

    /// `table.fill`: write the init funcref into `[dest, dest+len)`. The range is
    /// bounds-checked before any write (a trap on overflow, no partial write).
    pub(super) fn table_fill(&mut self, table: u32) -> Result<(), TranspileError> {
        let table = self.require_table_index(table)?;
        let (dest, val, len) = self.pop_bulk_operands()?;
        if table == 0 && !self.is_table64(table) {
            self.used_helpers.insert((Helper::TableFill, 0));
            self.line(format!(
                "self.table_fill(({}) as u32, ({}) as u32, ({}) as u32);",
                dest.code, val.code, len.code
            ));
            return Ok(());
        }
        let (_, get_mut) = table_accessor(table);
        self.line(format!(
            "{{ let d = {}; let v = ({}) as u32; let n = {}; self.{get_mut}()[d..d + n].fill(v); }}",
            self.table_index_expr(table, &dest.code),
            val.code,
            self.table_index_expr(table, &len.code)
        ));
        Ok(())
    }

    /// Whether segment `index` is still retained at run time. An active or
    /// declared segment is dropped at instantiation, so `*.init` reads it as empty
    /// and `*.drop` has nothing left to drop. An index outside the segment section
    /// is an error, not an empty segment.
    fn is_passive(&self, passive: &[bool], index: u32, kind: &str) -> Result<bool, TranspileError> {
        passive.get(index as usize).copied().ok_or_else(|| {
            TranspileError::Unsupported(format!("{kind} segment {index} does not exist"))
        })
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
        unchecked: bool,
    ) {
        // `unchecked` (opt-in `unsafe_memory`, memory.init only) drops the slice
        // bounds check on both the destination memory and the segment source,
        // turning an out-of-range copy into undefined behaviour rather than a
        // trap.
        let (dst_slice, seg_slice) = if unchecked {
            (
                format!("(*unsafe {{ {dst}.get_unchecked_mut(d..d + n) }})"),
                "unsafe { seg.get_unchecked(s..s + n) }".to_string(),
            )
        } else {
            (format!("{dst}[d..d + n]"), "&seg[s..s + n]".to_string())
        };
        self.line(format!(
            "{{ let seg = {src_seg}; let n = ({}) as usize; let s = ({}) as usize; \
             let d = ({}) as usize; {dst_slice}.copy_from_slice({seg_slice}); }}",
            len.code, src.code, dest.code
        ));
    }

    pub(super) fn memory_init(&mut self, data_index: u32, mem: u32) -> Result<(), TranspileError> {
        self.require_memory()?;
        let mem = self.require_memory_index(mem)?;
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
        // Unchecked memory access is opt-in and not wired through the shared
        // memory's lock, so it applies only to a non-shared memory.
        let unchecked = self.ctx.unsafe_memory && !self.ctx.memory_shared;
        // An active segment is already dropped, so it reads as an empty slice.
        let seg = if self.is_passive(&self.ctx.data_passive, data_index, "data")? {
            format!("self.data{data_index}")
        } else {
            "{ let empty: &[u8] = &[]; empty }".to_string()
        };
        self.emit_bulk_init(&dst, &seg, &dest, &src, &len, unchecked);
        Ok(())
    }

    pub(super) fn data_drop(&mut self, data_index: u32) -> Result<(), TranspileError> {
        // An active segment is dropped at instantiation, so dropping it again does
        // nothing.
        if self.is_passive(&self.ctx.data_passive, data_index, "data")? {
            self.line(format!("self.data{data_index} = &[];"));
        }
        Ok(())
    }

    pub(super) fn table_init(&mut self, elem_index: u32, table: u32) -> Result<(), TranspileError> {
        let table = self.require_table_index(table)?;
        let (dest, src, len) = self.pop_bulk_operands()?;
        let (_, get_mut) = table_accessor(table);
        // An active or declared segment is already dropped: an empty slice.
        let seg = if self.is_passive(&self.ctx.elem_passive, elem_index, "elem")? {
            format!("self.elem{elem_index}")
        } else {
            "{ let empty: &[u32] = &[]; empty }".to_string()
        };
        self.emit_bulk_init(
            &format!("self.{get_mut}()"),
            &seg,
            &dest,
            &src,
            &len,
            // Table storage is out of scope for unchecked memory access.
            false,
        );
        Ok(())
    }

    pub(super) fn elem_drop(&mut self, elem_index: u32) -> Result<(), TranspileError> {
        if self.is_passive(&self.ctx.elem_passive, elem_index, "elem")? {
            self.line(format!("self.elem{elem_index} = &[];"));
        }
        Ok(())
    }

    /// `ref.null t`: push a null reference, dispatching on the heap type. A
    /// struct/array (managed) type yields `GcRef::Null`; a `funcref`/`externref`
    /// yields the `u32::MAX` sentinel.
    pub(super) fn ref_null_dispatch(&mut self, hty: HeapType) -> Result<(), TranspileError> {
        match self.gc_type_index(hty) {
            Some(module_idx) => self.ref_null_gc(module_idx),
            // An abstract GC heap type (`any`/`eq`/`struct`/`array`/`none`) is a
            // managed `GcRef`, so its null is the `GcRef::Null` handle carried
            // with the abstract ref type.
            None if super::super::abstract_is_gc(hty) => {
                self.push(Val {
                    code: "GcRef::Null".to_string(),
                    ty: ValType::Ref(wasmparser::RefType::new(true, hty).ok_or_else(|| {
                        TranspileError::Unsupported(
                            "ref.null: cannot form abstract ref type".into(),
                        )
                    })?),
                    stable: true,
                    may_trap: false,
                });
                Ok(())
            }
            None => self.ref_null(hty),
        }
    }

    /// `ref.null t` for a `u32`-backed reference: push the `u32::MAX` sentinel.
    /// Covers the abstract `funcref`/`externref` and a concrete `(ref null $t)`
    /// whose type index names a function or continuation type (both lower to a
    /// `u32` handle).
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
            HeapType::Abstract {
                ty: AbstractHeapType::Exn,
                ..
            } => ValType::EXNREF,
            // A bottom type holds only null (it lowers like the top type of its
            // hierarchy) and `contref` is a `u32` handle: keep the type as written.
            hty if is_bottom_u32_heap(hty) || is_cont_heap(hty) => {
                ValType::Ref(RefType::new(true, hty).ok_or_else(|| {
                    TranspileError::Unsupported("ref.null: reference type".into())
                })?)
            }
            // A concrete funcref/contref type index: form the nullable concrete
            // ref type and carry the `u32::MAX` handle. (Struct/array indices are
            // handled earlier by `ref_null_gc`, so anything reaching here is a
            // `u32`-backed reference.)
            HeapType::Concrete(idx) => {
                let module_idx = idx.as_module_index().ok_or_else(|| {
                    TranspileError::Unsupported("ref.null: non-module type index".into())
                })?;
                let packed = PackedIndex::from_module_index(module_idx).ok_or_else(|| {
                    TranspileError::Unsupported("ref.null: type index too large".into())
                })?;
                ValType::Ref(RefType::concrete(true, packed))
            }
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
            may_trap: false,
        });
        Ok(())
    }

    /// `ref.func f`: push the funcref for function `f` (its full index).
    pub(super) fn ref_func(&mut self, function_index: u32) {
        self.push(Val {
            code: format!("{function_index}u32"),
            ty: ValType::FUNCREF,
            stable: true,
            may_trap: false,
        });
    }

    /// `cont.new $ct`: consume a `funcref` and produce a live continuation
    /// handle of type `(ref $ct)`. The handle is an index into the instance's
    /// `conts` table, allocated by the generated `cont_new` method, which builds
    /// the initial resumable frame for the referenced function. `u32::MAX`
    /// remains the null sentinel, so `ref.is_null` reports a fresh handle as
    /// non-null.
    pub(super) fn cont_new(&mut self, cont_type_index: u32) -> Result<(), TranspileError> {
        let idx = usize::try_from(cont_type_index)
            .map_err(|_| TranspileError::Unsupported("cont.new: type index too large".into()))?;
        match self.ctx.type_kinds.get(idx) {
            Some(CompositeKind::Cont(_)) => {}
            _ => {
                return Err(TranspileError::Unsupported(format!(
                    "cont.new: type index {cont_type_index} is not a continuation type"
                )));
            }
        }
        let func = self.pop()?;
        let packed = PackedIndex::from_module_index(cont_type_index)
            .ok_or_else(|| TranspileError::Unsupported("cont.new: type index too large".into()))?;
        let tmp = self.fresh_temp();
        self.line(format!(
            "let {tmp}: u32 = self.cont_new(({}) as u32);",
            func.code
        ));
        self.push(Val {
            code: tmp,
            ty: ValType::Ref(RefType::concrete(false, packed)),
            stable: true,
            may_trap: false,
        });
        Ok(())
    }

    /// `cont.bind $before $after`: partially apply a continuation's leading
    /// parameters. The `(ref $before)` continuation on top of the stack sits above
    /// the arguments to bind (`$before`'s leading parameters, the ones `$after`
    /// drops); pop them, `i64`-encode them, append them to the continuation's
    /// `bound` prefix, and push the same handle typed as `(ref $after)`. The next
    /// `resume` delivers the prefix ahead of its own arguments.
    pub(super) fn cont_bind(
        &mut self,
        before_index: u32,
        after_index: u32,
    ) -> Result<(), TranspileError> {
        let before =
            super::cont::cont_param_types(self.ctx.type_kinds, self.ctx.types, before_index)?;
        let after =
            super::cont::cont_param_types(self.ctx.type_kinds, self.ctx.types, after_index)?;
        // `$after` keeps a suffix of `$before`'s parameters; the bound arguments
        // are the leading ones it drops.
        let bound = before.len().checked_sub(after.len()).ok_or_else(|| {
            TranspileError::Unsupported(
                "cont.bind: result continuation has more parameters than the source".into(),
            )
        })?;
        if before[bound..] != after[..] {
            return Err(TranspileError::Unsupported(
                "cont.bind: result parameters are not a suffix of the source parameters".into(),
            ));
        }
        let after_packed = PackedIndex::from_module_index(after_index)
            .ok_or_else(|| TranspileError::Unsupported("cont.bind: type index too large".into()))?;

        let handle = self.pop()?;
        let handle_var = self.fresh_temp();
        self.line(format!("let {handle_var}: u32 = {};", handle.code));

        let bound_tys = &before[..bound];
        let mut args = Vec::with_capacity(bound);
        for _ in bound_tys {
            args.push(self.pop()?);
        }
        args.reverse();
        let mut encoded = Vec::with_capacity(bound);
        for (arg, ty) in args.iter().zip(bound_tys) {
            encoded.push(super::cont::encode_to_i64(&arg.code, *ty)?);
        }

        let result = self.fresh_temp();
        self.line(format!(
            "let {result}: u32 = self.cont_bind({handle_var}, &[{}]);",
            encoded.join(", ")
        ));
        self.push(Val {
            code: result,
            ty: ValType::Ref(RefType::concrete(false, after_packed)),
            stable: true,
            may_trap: false,
        });
        Ok(())
    }

    /// `ref.is_null`: pop a reference and push 1 if it is null, else 0. A managed
    /// `GcRef` compares against `GcRef::Null`; a `u32` funcref/externref compares
    /// against the `u32::MAX` sentinel.
    pub(super) fn ref_is_null(&mut self) -> Result<(), TranspileError> {
        let r = self.pop()?;
        let is_gc = super::super::rust_type(r.ty, self.ctx.type_kinds)? == "GcRef";
        let code = if is_gc {
            format!("(matches!({}, GcRef::Null) as i32)", r.code)
        } else {
            format!("i32::from({} == u32::MAX)", r.code)
        };
        self.push_combined(code, ValType::I32, &[&r])
    }

    /// The non-null `(ref i31)` value type carried by an `i31ref` on the operand
    /// stack. The payload lives in the `GcRef::I31` variant so an i31 unifies with
    /// the managed `any`/`eq` hierarchy.
    fn i31_ref_ty() -> Result<ValType, TranspileError> {
        let rt = RefType::new(
            false,
            HeapType::Abstract {
                shared: false,
                ty: AbstractHeapType::I31,
            },
        )
        .ok_or_else(|| TranspileError::Unsupported("cannot form i31 ref type".into()))?;
        Ok(ValType::Ref(rt))
    }

    /// `ref.i31`: narrow an `i32` to a 31-bit payload by masking off the top bit
    /// and box it as a `GcRef::I31` handle, so it can be stored in an `anyref`.
    /// Pure in its operand, so it keeps the operand's stability.
    pub(super) fn ref_i31(&mut self) -> Result<(), TranspileError> {
        let v = self.pop()?;
        self.push_combined(
            format!("GcRef::I31(({}) & 0x7FFF_FFFFi32)", v.code),
            Self::i31_ref_ty()?,
            &[&v],
        )
    }

    /// Emit an `i31.get_{s,u}`: read the `GcRef::I31` payload through
    /// `payload_expr` (in which `__v` is the raw i32 payload), trapping on a null
    /// handle. Keeps the operand's stability (a pure read).
    fn i31_get(&mut self, payload_expr: &str) -> Result<(), TranspileError> {
        let r = self.pop()?;
        self.push_combined(
            format!(
                "(match ({}) {{ GcRef::I31(__v) => {payload_expr}, \
                 _ => panic!(\"i31.get on null\") }})",
                r.code
            ),
            ValType::I32,
            &[&r],
        )
    }

    /// `i31.get_u`: read an `i31ref` payload zero-extended. The payload already
    /// has its top bit clear, so masking is harmless. A null handle traps.
    pub(super) fn i31_get_u(&mut self) -> Result<(), TranspileError> {
        self.i31_get("(__v) & 0x7FFF_FFFFi32")
    }

    /// `i31.get_s`: read an `i31ref` payload sign-extended from bit 30. Shifting
    /// left by one lands bit 30 in the sign position, then an arithmetic right
    /// shift replicates it back down. A null handle traps.
    pub(super) fn i31_get_s(&mut self) -> Result<(), TranspileError> {
        self.i31_get("((__v) << 1) >> 1")
    }
}
