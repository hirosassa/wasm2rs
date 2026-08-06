use wasmparser::{AbstractHeapType, HeapType, PackedIndex, RefType, ValType};

use super::super::helpers::mem_accessor;
use super::super::{CompositeKind, Helper, Val};
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
        self.push_combined(code, ValType::I32, r.stable)
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
        let stable = v.stable;
        self.push_combined(
            format!("GcRef::I31(({}) & 0x7FFF_FFFFi32)", v.code),
            Self::i31_ref_ty()?,
            stable,
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
            r.stable,
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
