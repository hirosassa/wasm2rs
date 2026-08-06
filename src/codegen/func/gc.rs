//! GC phase 4b: heap-allocated `struct` and `array` object operators.
//!
//! Every managed object is a `GcRef::Obj(Rc<RefCell<Vec<GcSlot>>>)` (see the
//! module-scope `GCREF_DEF`): a struct's slots are its fields in declaration
//! order, an array's slots are its elements. `struct.new`/`array.new` allocate;
//! the `get`/`set`/`len` operators dereference (trapping on a null handle via
//! `GcRef::obj()`), reading or writing a slot by index. Packed (`i8`/`i16`)
//! fields store their masked low bits in a `GcSlot::I32` and sign/zero-extend on
//! `*.get_s`/`*.get_u`.

use wasmparser::{AbstractHeapType, HeapType, PackedIndex, RefType, StorageType, ValType};

use super::super::{CompositeKind, FieldInfo, Val};
use crate::TranspileError;

/// Which slot variant backs a storage type, and how to wrap/unwrap it.
enum SlotShape {
    /// A packed field (`i8`/`i16`): stored masked in a `GcSlot::I32`. Carries the
    /// low-bit mask and the extension shift amount for signed reads.
    Packed { mask: u32, shift: u32 },
    /// A `funcref`/`externref` field (represented as a `u32`), stored in a
    /// `GcSlot::Func`. Carries the field's ref value type so a read yields it.
    Func(ValType),
    /// A full value type stored in its matching slot variant.
    Val(ValType),
}

impl SlotShape {
    fn of(storage: StorageType, kinds: &[CompositeKind]) -> Result<Self, TranspileError> {
        Ok(match storage {
            StorageType::I8 => SlotShape::Packed {
                mask: 0xFF,
                shift: 24,
            },
            StorageType::I16 => SlotShape::Packed {
                mask: 0xFFFF,
                shift: 16,
            },
            // A funcref/externref (abstract or a concrete function-type index)
            // lowers to a `u32`, so it needs the `GcSlot::Func` slot rather than
            // the `GcSlot::Ref` (`GcRef`) one used by managed struct/array refs.
            StorageType::Val(ty @ ValType::Ref(_))
                if super::super::rust_type(ty, kinds)? == "u32" =>
            {
                SlotShape::Func(ty)
            }
            StorageType::Val(ty) => SlotShape::Val(ty),
        })
    }

    /// The `GcSlot` variant name for a full value type.
    fn val_variant(ty: ValType) -> &'static str {
        match ty {
            ValType::I32 => "I32",
            ValType::I64 => "I64",
            ValType::F32 => "F32",
            ValType::F64 => "F64",
            ValType::V128 => "V128",
            ValType::Ref(_) => "Ref",
        }
    }

    /// The Rust expression constructing the slot holding `value`.
    fn wrap(&self, value: &str) -> Result<String, TranspileError> {
        Ok(match self {
            SlotShape::Packed { mask, .. } => format!("GcSlot::I32(({value}) & {mask:#X})"),
            SlotShape::Func(_) => format!("GcSlot::Func({value})"),
            SlotShape::Val(ValType::Ref(_)) => format!("GcSlot::Ref({value})"),
            SlotShape::Val(ty) => format!("GcSlot::{}({value})", Self::val_variant(*ty)),
        })
    }

    /// A `match &slot { <arm> }` reading the slot back as its unpacked value.
    /// `slot` is the borrowed slot expression. Returns `(read_expr, result_ty)`
    /// where the read has not yet been sign/zero-extended for a packed field.
    fn read(&self, slot: &str) -> Result<(String, ValType), TranspileError> {
        Ok(match self {
            SlotShape::Packed { .. } => (
                format!("match {slot} {{ GcSlot::I32(v) => *v, _ => unreachable!() }}"),
                ValType::I32,
            ),
            SlotShape::Func(ty) => (
                format!("match {slot} {{ GcSlot::Func(v) => *v, _ => unreachable!() }}"),
                *ty,
            ),
            SlotShape::Val(ValType::Ref(_)) => {
                let ty = self.result_ty();
                (
                    format!("match {slot} {{ GcSlot::Ref(v) => v.clone(), _ => unreachable!() }}"),
                    ty,
                )
            }
            SlotShape::Val(ty) => (
                format!(
                    "match {slot} {{ GcSlot::{}(v) => *v, _ => unreachable!() }}",
                    Self::val_variant(*ty)
                ),
                *ty,
            ),
        })
    }

    /// For `array.new_data`/`array.init_data`: the element's byte width and the
    /// `GcSlot` expression reading one little-endian element from slice `seg` at
    /// byte index `base`. A reference element type has no in-memory byte encoding,
    /// so it is rejected (that is what `*.new_elem`/`*.init_elem` are for).
    fn read_le_bytes(&self, seg: &str, base: &str) -> Result<(usize, String), TranspileError> {
        let le = |n: usize| {
            (0..n)
                .map(|k| format!("{seg}[{base} + {k}]"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        Ok(match self {
            SlotShape::Packed { mask: 0xFF, .. } => {
                (1, format!("GcSlot::I32(({seg}[{base}] as i32) & 0xFF)"))
            }
            SlotShape::Packed { mask: 0xFFFF, .. } => (
                2,
                format!(
                    "GcSlot::I32((u16::from_le_bytes([{}]) as i32) & 0xFFFF)",
                    le(2)
                ),
            ),
            SlotShape::Packed { .. } => {
                return Err(TranspileError::Unsupported(
                    "array.new_data with an unexpected packed element".into(),
                ));
            }
            SlotShape::Val(ValType::I32) => {
                (4, format!("GcSlot::I32(i32::from_le_bytes([{}]))", le(4)))
            }
            SlotShape::Val(ValType::I64) => {
                (8, format!("GcSlot::I64(i64::from_le_bytes([{}]))", le(8)))
            }
            SlotShape::Val(ValType::F32) => {
                (4, format!("GcSlot::F32(f32::from_le_bytes([{}]))", le(4)))
            }
            SlotShape::Val(ValType::F64) => {
                (8, format!("GcSlot::F64(f64::from_le_bytes([{}]))", le(8)))
            }
            SlotShape::Val(ValType::V128) => (
                16,
                format!("GcSlot::V128(u128::from_le_bytes([{}]))", le(16)),
            ),
            SlotShape::Func(_) | SlotShape::Val(ValType::Ref(_)) => {
                return Err(TranspileError::Unsupported(
                    "array.new_data/init_data with a reference element type".into(),
                ));
            }
        })
    }

    /// The value type the plain (`.get`, no packed extension) read yields.
    fn result_ty(&self) -> ValType {
        match self {
            SlotShape::Packed { .. } => ValType::I32,
            SlotShape::Func(ty) | SlotShape::Val(ty) => *ty,
        }
    }

    /// The Rust expression for the default `GcSlot` of this storage type: the
    /// zero of a numeric (packed fields default to `GcSlot::I32(0)`), the null
    /// (`u32::MAX`) funcref/externref for a `Func` slot, and the managed null
    /// handle for a GC reference.
    fn default_slot(&self) -> Result<&'static str, TranspileError> {
        Ok(match self {
            SlotShape::Packed { .. } => "GcSlot::I32(0)",
            SlotShape::Func(_) => "GcSlot::Func(u32::MAX)",
            SlotShape::Val(ValType::I32) => "GcSlot::I32(0)",
            SlotShape::Val(ValType::I64) => "GcSlot::I64(0)",
            SlotShape::Val(ValType::F32) => "GcSlot::F32(0.0)",
            SlotShape::Val(ValType::F64) => "GcSlot::F64(0.0)",
            SlotShape::Val(ValType::V128) => "GcSlot::V128(0)",
            SlotShape::Val(ValType::Ref(_)) => "GcSlot::Ref(GcRef::Null)",
        })
    }
}

impl super::FuncGen<'_> {
    /// Resolve a struct type index to its field list, erroring if the index is
    /// not a struct type.
    fn struct_fields(&self, type_index: u32) -> Result<&[FieldInfo], TranspileError> {
        match self.ctx.type_kinds.get(type_index as usize) {
            Some(CompositeKind::Struct(fields)) => Ok(fields),
            _ => Err(TranspileError::Unsupported(format!(
                "type {type_index} is not a struct"
            ))),
        }
    }

    /// Resolve an array type index to its element field, erroring otherwise.
    fn array_element(&self, type_index: u32) -> Result<&FieldInfo, TranspileError> {
        match self.ctx.type_kinds.get(type_index as usize) {
            Some(CompositeKind::Array(elem)) => Ok(elem),
            _ => Err(TranspileError::Unsupported(format!(
                "type {type_index} is not an array"
            ))),
        }
    }

    /// The [`SlotShape`] backing array type `type_index`'s element.
    fn array_element_shape(&self, type_index: u32) -> Result<SlotShape, TranspileError> {
        SlotShape::of(self.array_element(type_index)?.storage, self.ctx.type_kinds)
    }

    /// The [`SlotShape`] backing field `field_index` of struct type `type_index`.
    fn struct_field_shape(
        &self,
        type_index: u32,
        field_index: u32,
    ) -> Result<SlotShape, TranspileError> {
        let field = self
            .struct_fields(type_index)?
            .get(field_index as usize)
            .ok_or_else(|| {
                TranspileError::Unsupported(format!("struct field {field_index} out of range"))
            })?;
        SlotShape::of(field.storage, self.ctx.type_kinds)
    }

    /// The module type index of a concrete struct/array heap type, or `None` for
    /// a funcref/externref/abstract or otherwise non-managed heap type.
    pub(super) fn gc_type_index(&self, hty: wasmparser::HeapType) -> Option<u32> {
        let wasmparser::HeapType::Concrete(idx) = hty else {
            return None;
        };
        let module_idx = idx.as_module_index()?;
        match self.ctx.type_kinds.get(module_idx as usize) {
            Some(CompositeKind::Struct(_) | CompositeKind::Array(_)) => Some(module_idx),
            _ => None,
        }
    }

    /// The value type of a non-null concrete reference to type index `ti`,
    /// lowering to `GcRef` for a struct/array type.
    fn concrete_ref_ty(type_index: u32) -> Result<ValType, TranspileError> {
        let idx = PackedIndex::from_module_index(type_index)
            .ok_or_else(|| TranspileError::Unsupported("type index too large".into()))?;
        Ok(ValType::Ref(wasmparser::RefType::concrete(false, idx)))
    }

    /// `struct.new $t`: pop the N fields (last field on top) and allocate an
    /// object whose slots hold them in declaration order.
    pub(super) fn struct_new(&mut self, type_index: u32) -> Result<(), TranspileError> {
        let shapes = self
            .struct_fields(type_index)?
            .iter()
            .map(|f| SlotShape::of(f.storage, self.ctx.type_kinds))
            .collect::<Result<Vec<_>, _>>()?;
        // Freeze anything below the fields, then pop the fields (top is last).
        self.freeze_survivors(shapes.len())?;
        let mut slots = vec![String::new(); shapes.len()];
        for (i, shape) in shapes.iter().enumerate().rev() {
            let v = self.pop()?;
            slots[i] = shape.wrap(&v.code)?;
        }
        let obj = format!(
            "GcRef::Obj {{ ty: {type_index}u32, \
             slots: std::rc::Rc::new(std::cell::RefCell::new(vec![{}])) }}",
            slots.join(", ")
        );
        // An allocation is not re-evaluatable, so bind it to a stable temp.
        self.materialize(obj, Self::concrete_ref_ty(type_index)?)
    }

    /// `struct.get{,_s,_u} $t f`: pop the ref and read field `f`. `ext` selects
    /// the packed extension (`None` for a full read, `Some(true)` sign-, `Some
    /// (false)` zero-extend). A null ref traps via `GcRef::obj()`.
    pub(super) fn struct_get(
        &mut self,
        type_index: u32,
        field_index: u32,
        ext: Option<bool>,
    ) -> Result<(), TranspileError> {
        let shape = self.struct_field_shape(type_index, field_index)?;
        let r = self.pop()?;
        let (read, ty) = shape.read(&format!("&__b[{field_index}]"))?;
        let inner = format!(
            "{{ let __o = ({}).obj(); let __b = __o.borrow(); {read} }}",
            r.code
        );
        let (code, ty) = extend_packed(&shape, inner, ty, ext);
        // The read dereferences a managed object; bind it to a stable temp.
        self.materialize(code, ty)
    }

    /// `struct.set $t f`: pop the value then the ref and store into field `f`.
    pub(super) fn struct_set(
        &mut self,
        type_index: u32,
        field_index: u32,
    ) -> Result<(), TranspileError> {
        let shape = self.struct_field_shape(type_index, field_index)?;
        self.freeze_survivors(2)?;
        let value = self.pop()?;
        let r = self.pop()?;
        let slot = shape.wrap(&value.code)?;
        self.line(format!(
            "{{ let __o = ({}).obj(); __o.borrow_mut()[{field_index}] = {slot}; }}",
            r.code
        ));
        Ok(())
    }

    /// `array.new $t`: pop `size` (top) then the init value and allocate an array
    /// of `size` copies of the init value.
    pub(super) fn array_new(&mut self, type_index: u32) -> Result<(), TranspileError> {
        let shape = self.array_element_shape(type_index)?;
        self.freeze_survivors(2)?;
        let size = self.pop()?;
        let init = self.pop()?;
        let slot = shape.wrap(&init.code)?;
        let code = format!(
            "{{ let __n = ({}) as usize; \
             GcRef::Obj {{ ty: {type_index}u32, \
             slots: std::rc::Rc::new(std::cell::RefCell::new(vec![{slot}; __n])) }} }}",
            size.code
        );
        self.materialize(code, Self::concrete_ref_ty(type_index)?)
    }

    /// `array.get{,_s,_u} $t`: pop `index` (top) then the ref and read the
    /// element. A null ref or an out-of-bounds index traps.
    pub(super) fn array_get(
        &mut self,
        type_index: u32,
        ext: Option<bool>,
    ) -> Result<(), TranspileError> {
        let shape = self.array_element_shape(type_index)?;
        self.freeze_survivors(2)?;
        let index = self.pop()?;
        let r = self.pop()?;
        let (read, ty) = shape.read("&__b[(__i) as usize]")?;
        let inner = format!(
            "{{ let __o = ({}).obj(); let __i = {}; let __b = __o.borrow(); {read} }}",
            r.code, index.code
        );
        let (code, ty) = extend_packed(&shape, inner, ty, ext);
        self.materialize(code, ty)
    }

    /// `array.set $t`: pop `value` (top), then `index`, then the ref.
    pub(super) fn array_set(&mut self, type_index: u32) -> Result<(), TranspileError> {
        let shape = self.array_element_shape(type_index)?;
        self.freeze_survivors(3)?;
        let value = self.pop()?;
        let index = self.pop()?;
        let r = self.pop()?;
        let slot = shape.wrap(&value.code)?;
        self.line(format!(
            "{{ let __o = ({}).obj(); __o.borrow_mut()[({}) as usize] = {slot}; }}",
            r.code, index.code
        ));
        Ok(())
    }

    /// `array.len`: pop the ref and push the element count as an `i32`.
    pub(super) fn array_len(&mut self) -> Result<(), TranspileError> {
        let r = self.pop()?;
        self.push_combined(
            format!("(({}).obj().borrow().len() as i32)", r.code),
            ValType::I32,
            false,
        )
    }

    /// `struct.new_default $t`: allocate an object whose slots are each field's
    /// default value (0 for numerics, a null handle for a GC-ref field).
    pub(super) fn struct_new_default(&mut self, type_index: u32) -> Result<(), TranspileError> {
        let defaults = self
            .struct_fields(type_index)?
            .iter()
            .map(|f| SlotShape::of(f.storage, self.ctx.type_kinds).and_then(|s| s.default_slot()))
            .collect::<Result<Vec<_>, _>>()?;
        let obj = format!(
            "GcRef::Obj {{ ty: {type_index}u32, \
             slots: std::rc::Rc::new(std::cell::RefCell::new(vec![{}])) }}",
            defaults.join(", ")
        );
        // An allocation is not re-evaluatable, so bind it to a stable temp.
        self.materialize(obj, Self::concrete_ref_ty(type_index)?)
    }

    /// `array.new_default $t`: pop `size` (top) and allocate an array of `size`
    /// default (0 / null) elements.
    pub(super) fn array_new_default(&mut self, type_index: u32) -> Result<(), TranspileError> {
        let default = self.array_element_shape(type_index)?.default_slot()?;
        let size = self.pop()?;
        let code = format!(
            "{{ let __n = ({}) as usize; \
             GcRef::Obj {{ ty: {type_index}u32, \
             slots: std::rc::Rc::new(std::cell::RefCell::new(vec![{default}; __n])) }} }}",
            size.code
        );
        self.materialize(code, Self::concrete_ref_ty(type_index)?)
    }

    /// `array.new_fixed $t N`: pop the `N` elements (last on top) and allocate an
    /// array holding them in element order.
    pub(super) fn array_new_fixed(
        &mut self,
        type_index: u32,
        array_size: u32,
    ) -> Result<(), TranspileError> {
        let shape = self.array_element_shape(type_index)?;
        let n = array_size as usize;
        // Freeze anything below the elements, then pop them (top is last).
        self.freeze_survivors(n)?;
        let mut slots = vec![String::new(); n];
        for i in (0..n).rev() {
            let v = self.pop()?;
            slots[i] = shape.wrap(&v.code)?;
        }
        let obj = format!(
            "GcRef::Obj {{ ty: {type_index}u32, \
             slots: std::rc::Rc::new(std::cell::RefCell::new(vec![{}])) }}",
            slots.join(", ")
        );
        self.materialize(obj, Self::concrete_ref_ty(type_index)?)
    }

    /// `array.fill $t`: operand stack `[arrayref, offset, value, size]` (`size`
    /// on top). Write `size` copies of `value` into the array starting at
    /// `offset`. An out-of-range write traps.
    pub(super) fn array_fill(&mut self, type_index: u32) -> Result<(), TranspileError> {
        let shape = self.array_element_shape(type_index)?;
        self.freeze_survivors(4)?;
        let size = self.pop()?;
        let value = self.pop()?;
        let offset = self.pop()?;
        let r = self.pop()?;
        let slot = shape.wrap(&value.code)?;
        self.line(format!(
            "{{ let __o = ({}).obj(); let mut __b = __o.borrow_mut(); \
             let __off = ({}) as usize; let __n = ({}) as usize; let __v = {slot}; \
             for __k in 0..__n {{ __b[__off + __k] = __v.clone(); }} }}",
            r.code, offset.code, size.code
        ));
        Ok(())
    }

    /// `array.copy $t_dst $t_src`: operand stack
    /// `[destref, dest_offset, srcref, src_offset, size]` (`size` on top). Copy
    /// `size` elements from the source range to the destination range. The source
    /// range is snapshotted before the destination is borrowed mutably so a
    /// self-copy (both handles backed by the same `Rc`) neither double-borrows nor
    /// corrupts on overlap. Out-of-range indexing traps.
    pub(super) fn array_copy(&mut self, type_index_dst: u32) -> Result<(), TranspileError> {
        // Validate the destination element type is representable; the whole
        // `GcSlot`s are copied, so no per-element rewrap is needed.
        self.array_element_shape(type_index_dst)?;
        self.freeze_survivors(5)?;
        let size = self.pop()?;
        let src_offset = self.pop()?;
        let srcref = self.pop()?;
        let dest_offset = self.pop()?;
        let destref = self.pop()?;
        self.line(format!(
            "{{ let __so = ({}).obj(); let __sb = __so.borrow(); \
             let __si = ({}) as usize; let __n = ({}) as usize; \
             let __seg: Vec<GcSlot> = __sb[__si .. __si + __n].to_vec(); drop(__sb); \
             let __do = ({}).obj(); let mut __db = __do.borrow_mut(); \
             let __di = ({}) as usize; \
             for (__k, __e) in __seg.into_iter().enumerate() {{ __db[__di + __k] = __e; }} }}",
            srcref.code, src_offset.code, size.code, destref.code, dest_offset.code
        ));
        Ok(())
    }

    /// `array.new_data $t $d`: operand stack `[offset, size]` (`size` on top).
    /// Read `size` little-endian numeric elements from passive data segment `$d`,
    /// starting at byte `offset`, into a fresh array. An out-of-range read of the
    /// (retained, so drop-aware) segment traps.
    pub(super) fn array_new_data(
        &mut self,
        type_index: u32,
        data_index: u32,
    ) -> Result<(), TranspileError> {
        self.require_passive(&self.ctx.data_passive, data_index, "data")?;
        let shape = self.array_element_shape(type_index)?;
        let (esize, slot) = shape.read_le_bytes("__seg", "__base")?;
        self.freeze_survivors(2)?;
        let size = self.pop()?;
        let offset = self.pop()?;
        let code = format!(
            "{{ let __seg = self.data{data_index}; \
             let __off = ({}) as usize; let __n = ({}) as usize; \
             let mut __v: Vec<GcSlot> = Vec::with_capacity(__n); \
             for __k in 0..__n {{ let __base = __off + __k * {esize}; __v.push({slot}); }} \
             GcRef::Obj {{ ty: {type_index}u32, \
             slots: std::rc::Rc::new(std::cell::RefCell::new(__v)) }} }}",
            offset.code, size.code
        );
        self.materialize(code, Self::concrete_ref_ty(type_index)?)
    }

    /// `array.init_data $t $d`: operand stack `[arrayref, dest, src, size]`
    /// (`size` on top). Copy `size` elements from passive data segment `$d` (from
    /// byte `src`) into the array starting at element `dest`. Out-of-range
    /// indexing on either side traps.
    pub(super) fn array_init_data(
        &mut self,
        type_index: u32,
        data_index: u32,
    ) -> Result<(), TranspileError> {
        self.require_passive(&self.ctx.data_passive, data_index, "data")?;
        let shape = self.array_element_shape(type_index)?;
        let (esize, slot) = shape.read_le_bytes("__seg", "__base")?;
        self.freeze_survivors(4)?;
        let size = self.pop()?;
        let src = self.pop()?;
        let dest = self.pop()?;
        let r = self.pop()?;
        self.line(format!(
            "{{ let __o = ({}).obj(); let mut __b = __o.borrow_mut(); \
             let __seg = self.data{data_index}; \
             let __d = ({}) as usize; let __s = ({}) as usize; let __n = ({}) as usize; \
             for __k in 0..__n {{ let __base = __s + __k * {esize}; __b[__d + __k] = {slot}; }} }}",
            r.code, dest.code, src.code, size.code
        ));
        Ok(())
    }

    /// `array.new_elem $t $e`: operand stack `[offset, size]` (`size` on top).
    /// Read `size` funcrefs from passive element segment `$e`, starting at index
    /// `offset`, into a fresh array. The array element type must be a funcref
    /// (`GcSlot::Func`). An out-of-range read traps.
    pub(super) fn array_new_elem(
        &mut self,
        type_index: u32,
        elem_index: u32,
    ) -> Result<(), TranspileError> {
        self.require_passive(&self.ctx.elem_passive, elem_index, "elem")?;
        self.require_func_element(type_index, "array.new_elem")?;
        self.freeze_survivors(2)?;
        let size = self.pop()?;
        let offset = self.pop()?;
        let code = format!(
            "{{ let __seg = self.elem{elem_index}; \
             let __off = ({}) as usize; let __n = ({}) as usize; \
             let mut __v: Vec<GcSlot> = Vec::with_capacity(__n); \
             for __k in 0..__n {{ __v.push(GcSlot::Func(__seg[__off + __k])); }} \
             GcRef::Obj {{ ty: {type_index}u32, \
             slots: std::rc::Rc::new(std::cell::RefCell::new(__v)) }} }}",
            offset.code, size.code
        );
        self.materialize(code, Self::concrete_ref_ty(type_index)?)
    }

    /// `array.init_elem $t $e`: operand stack `[arrayref, dest, src, size]`
    /// (`size` on top). Copy `size` funcrefs from passive element segment `$e`
    /// (from index `src`) into the array starting at element `dest`. The array
    /// element type must be a funcref. Out-of-range indexing traps.
    pub(super) fn array_init_elem(
        &mut self,
        type_index: u32,
        elem_index: u32,
    ) -> Result<(), TranspileError> {
        self.require_passive(&self.ctx.elem_passive, elem_index, "elem")?;
        self.require_func_element(type_index, "array.init_elem")?;
        self.freeze_survivors(4)?;
        let size = self.pop()?;
        let src = self.pop()?;
        let dest = self.pop()?;
        let r = self.pop()?;
        self.line(format!(
            "{{ let __o = ({}).obj(); let mut __b = __o.borrow_mut(); \
             let __seg = self.elem{elem_index}; \
             let __d = ({}) as usize; let __s = ({}) as usize; let __n = ({}) as usize; \
             for __k in 0..__n {{ __b[__d + __k] = GcSlot::Func(__seg[__s + __k]); }} }}",
            r.code, dest.code, src.code, size.code
        ));
        Ok(())
    }

    /// Require array type `type_index`'s element to be a funcref/externref
    /// (`GcSlot::Func`-backed), the only element an element segment can fill.
    fn require_func_element(&self, type_index: u32, op: &str) -> Result<(), TranspileError> {
        let shape = self.array_element_shape(type_index)?;
        if matches!(shape, SlotShape::Func(_)) {
            Ok(())
        } else {
            Err(TranspileError::Unsupported(format!(
                "{op} needs a funcref array element type"
            )))
        }
    }

    /// The `matches!(<ty_expr>, d0 | d1 | ...)` membership test deciding whether
    /// a runtime concrete type id (`ty_expr`) is a subtype of the static target
    /// `T` — i.e. is in `T`'s descendant set. An empty set yields `false` (no
    /// concrete type can match), avoiding an empty `matches!` pattern.
    fn subtype_member(&self, ty_expr: &str, target: u32) -> String {
        let desc = self.ctx.concrete_descendants(target);
        if desc.is_empty() {
            return "false".to_string();
        }
        let arms = desc
            .iter()
            .map(|d| format!("{d}u32"))
            .collect::<Vec<_>>()
            .join(" | ");
        format!("matches!({ty_expr}, {arms})")
    }

    /// A `matches!(*ty, ...)` over the runtime type id of a `GcRef::Obj`, true
    /// when that id names a struct type (`want_struct`) or an array type. Backs
    /// the abstract `struct`/`array` heap-type checks. An empty set yields
    /// `"false"` to avoid an empty pattern.
    fn abstract_obj_member(&self, want_struct: bool) -> Result<String, TranspileError> {
        let mut arms = Vec::new();
        for (i, kind) in self.ctx.type_kinds.iter().enumerate() {
            let hit = match kind {
                CompositeKind::Struct(_) => want_struct,
                CompositeKind::Array(_) => !want_struct,
                CompositeKind::Func => false,
            };
            if hit {
                arms.push(format!("{}u32", super::super::index_u32(i)?));
            }
        }
        if arms.is_empty() {
            return Ok("false".to_string());
        }
        Ok(format!("matches!(*ty, {})", arms.join(" | ")))
    }

    /// A `match &<ref_expr> { ... }` yielding a Rust `bool`: whether the ref's
    /// runtime type is a subtype of the target heap type `hty`. Handles both a
    /// concrete struct/array target and the abstract GC heap types
    /// (`any`/`eq`/`i31`/`struct`/`array`/`none`); the `i31`, `struct` and
    /// `array` cases separate the `GcRef::I31` and `GcRef::Obj` variants.
    /// `null_matches` picks how the null handle is classified (a nullable target
    /// matches null). A `func`/`extern` target is unsupported here.
    fn gc_heap_test(
        &self,
        ref_expr: &str,
        hty: HeapType,
        null_matches: bool,
    ) -> Result<String, TranspileError> {
        // `(i31_arm, obj_arm)`: how a `GcRef::I31` payload and a `GcRef::Obj`
        // (whose runtime type id is bound as `ty`) each answer the test.
        let (i31_arm, obj_arm) = match hty {
            HeapType::Abstract {
                ty: AbstractHeapType::Any | AbstractHeapType::Eq,
                ..
            } => ("true".to_string(), "true".to_string()),
            HeapType::Abstract {
                ty: AbstractHeapType::I31,
                ..
            } => ("true".to_string(), "false".to_string()),
            HeapType::Abstract {
                ty: AbstractHeapType::Struct,
                ..
            } => ("false".to_string(), self.abstract_obj_member(true)?),
            HeapType::Abstract {
                ty: AbstractHeapType::Array,
                ..
            } => ("false".to_string(), self.abstract_obj_member(false)?),
            HeapType::Abstract {
                ty: AbstractHeapType::None,
                ..
            } => ("false".to_string(), "false".to_string()),
            HeapType::Concrete(_) => {
                let target = self.gc_type_index(hty).ok_or_else(|| {
                    TranspileError::Unsupported(
                        "ref cast/test to a non-struct/array concrete type".into(),
                    )
                })?;
                ("false".to_string(), self.subtype_member("*ty", target))
            }
            _ => {
                return Err(TranspileError::Unsupported(
                    "ref cast/test on a func/extern heap type".into(),
                ));
            }
        };
        Ok(format!(
            "match &({ref_expr}) {{ GcRef::Null => {null_matches}, \
             GcRef::I31(_) => {i31_arm}, GcRef::Obj {{ ty, .. }} => {obj_arm} }}"
        ))
    }

    /// The value type of a `ref.cast` result: the target heap type at the cast's
    /// nullability, held as a `GcRef`.
    fn cast_result_ty(hty: HeapType, nullable: bool) -> Result<ValType, TranspileError> {
        let rt = RefType::new(nullable, hty).ok_or_else(|| {
            TranspileError::Unsupported("cannot form cast result ref type".into())
        })?;
        Ok(ValType::Ref(rt))
    }

    /// `ref.test`: pop the ref and push an `i32` (`1`/`0`) reporting whether its
    /// runtime type is a subtype of the target heap type. `null_matches` is set
    /// for the nullable variant (`ref.test (ref null $t)`) and clear for the
    /// non-null variant, which the surrounding operator dispatch selects.
    pub(super) fn ref_test(
        &mut self,
        hty: wasmparser::HeapType,
        null_matches: bool,
    ) -> Result<(), TranspileError> {
        let r = self.pop()?;
        let test = self.gc_heap_test(&r.code, hty, null_matches)?;
        self.materialize(format!("i32::from({test})"), ValType::I32)
    }

    /// `ref.cast`: pop the ref, trap if its runtime type is not a subtype of the
    /// target, and push the same handle typed as the target reference. The
    /// nullability of the target (`null_matches`) decides whether null passes.
    pub(super) fn ref_cast(
        &mut self,
        hty: wasmparser::HeapType,
        null_matches: bool,
    ) -> Result<(), TranspileError> {
        let r = self.pop()?;
        let test = self.gc_heap_test("__r", hty, null_matches)?;
        let code = format!(
            "{{ let __r = ({}); if {test} {{ __r }} else {{ panic!(\"ref.cast failed\") }} }}",
            r.code
        );
        self.materialize(code, Self::cast_result_ty(hty, null_matches)?)
    }

    /// `br_on_cast`/`br_on_cast_fail`: branch to `depth` on (respectively) a
    /// successful/failed downcast of the stack-top ref to `to_ref_type`, leaving
    /// it on the stack for the fall-through path. Both `from`/`to` ref types
    /// lower to `GcRef`, so only the stack `Val.ty` differs between the paths.
    pub(super) fn br_on_cast(
        &mut self,
        depth: u32,
        from_ref_type: wasmparser::RefType,
        to_ref_type: wasmparser::RefType,
        on_success: bool,
    ) -> Result<(), TranspileError> {
        // Pin the ref into a stable temp so the branch carries it on the taken
        // path and leaves the same value on the stack for fall-through, without
        // re-evaluating (and so re-allocating) the operand.
        let r = self.pop()?;
        let temp = self.fresh_temp();
        self.line(format!("let {temp}: GcRef = {};", r.code));
        // The condition tests the runtime type against `to_ref_type` (whose
        // nullability decides how null is treated). `br_on_cast` branches on a
        // match; `br_on_cast_fail` branches on the complement.
        let matches =
            self.gc_heap_test(&temp, to_ref_type.heap_type(), to_ref_type.is_nullable())?;
        let cond = if on_success {
            format!("i32::from({matches})")
        } else {
            format!("i32::from(!({matches}))")
        };
        // The fall-through type is the one *not* carried on the branch.
        let fallthrough_ty = if on_success {
            ValType::Ref(from_ref_type)
        } else {
            ValType::Ref(to_ref_type)
        };
        self.push(Val {
            code: temp,
            ty: fallthrough_ty,
            stable: true,
        });
        self.branch(
            depth,
            Some(Val {
                code: cond,
                ty: ValType::I32,
                stable: true,
            }),
        )
    }

    /// `ref.null` of a struct/array (or an abstract GC) heap type: push the
    /// managed null handle.
    pub(super) fn ref_null_gc(&mut self, type_index: u32) -> Result<(), TranspileError> {
        self.push(Val {
            code: "GcRef::Null".to_string(),
            ty: Self::concrete_ref_ty(type_index)?,
            stable: true,
        });
        Ok(())
    }

    /// `ref.eq`: pop two `eqref` handles and push `1`/`0` for identity equality.
    /// Two nulls are equal; two objects compare by `Rc` pointer identity on their
    /// shared slots; two `i31` handles compare by payload; any other mix (null vs
    /// object, i31 vs object, …) is unequal.
    pub(super) fn ref_eq(&mut self) -> Result<(), TranspileError> {
        let b = self.pop()?;
        let a = self.pop()?;
        let code = format!(
            "(match (&({}), &({})) {{ \
             (GcRef::Null, GcRef::Null) => 1i32, \
             (GcRef::I31(__x), GcRef::I31(__y)) => i32::from(__x == __y), \
             (GcRef::Obj {{ slots: __x, .. }}, GcRef::Obj {{ slots: __y, .. }}) => \
             i32::from(std::rc::Rc::ptr_eq(__x, __y)), \
             _ => 0i32 }})",
            a.code, b.code
        );
        self.push_combined(code, ValType::I32, false)
    }

    /// `extern.convert_any`: internalise an `anyref` into an `externref`. Null
    /// maps to the null externref (`u32::MAX`); a managed handle is pushed onto
    /// the per-instance `extern_box` and its index returned as the `u32`
    /// externref, so `any.convert_extern` can recover the same handle.
    pub(super) fn extern_convert_any(&mut self) -> Result<(), TranspileError> {
        let a = self.pop()?;
        // Borrow (not move) the operand: it may be a local reused afterwards.
        let code = format!(
            "{{ match &({}) {{ GcRef::Null => u32::MAX, \
             __x => {{ let __i = self.extern_box.len() as u32; \
             self.extern_box.push(__x.clone()); __i }} }} }}",
            a.code
        );
        self.materialize(code, ValType::EXTERNREF)
    }

    /// `any.convert_extern`: externalise an `externref` back into an `anyref`.
    /// The null externref (`u32::MAX`) maps to `GcRef::Null`; any other value is
    /// an index into `extern_box` (produced by `extern.convert_any`) whose handle
    /// is cloned back out. A host-provided externref is out of scope (it would
    /// index outside the box and trap).
    pub(super) fn any_convert_extern(&mut self) -> Result<(), TranspileError> {
        let e = self.pop()?;
        let rt = RefType::new(
            true,
            HeapType::Abstract {
                shared: false,
                ty: AbstractHeapType::Any,
            },
        )
        .ok_or_else(|| TranspileError::Unsupported("cannot form anyref type".into()))?;
        let code = format!(
            "{{ let __e = ({}); if __e == u32::MAX {{ GcRef::Null }} \
             else {{ self.extern_box[__e as usize].clone() }} }}",
            e.code
        );
        self.materialize(code, ValType::Ref(rt))
    }

    /// `ref.as_non_null`: pop a nullable ref, trap on null, else pass the handle
    /// through typed as a non-null ref (still a `GcRef`).
    pub(super) fn ref_as_non_null(&mut self) -> Result<(), TranspileError> {
        let r = self.pop()?;
        let ty = r.ty;
        let code = format!(
            "{{ let __r = ({}); \
             if matches!(__r, GcRef::Null) {{ panic!(\"ref.as_non_null on null\") }} else {{ __r }} }}",
            r.code
        );
        self.materialize(code, ty)
    }

    /// `br_on_null $l`: branch to `$l` when the popped ref is null (the ref is
    /// dropped, so the branch carries only the target block's other values), else
    /// fall through leaving the now-non-null ref on the stack.
    pub(super) fn br_on_null(&mut self, depth: u32) -> Result<(), TranspileError> {
        // Pin the ref into a stable temp: it is dropped on the null (taken) path
        // and pushed back for fall-through, so it must not be re-evaluated.
        let r = self.pop()?;
        let ty = r.ty;
        let temp = self.fresh_temp();
        self.line(format!("let {temp}: GcRef = {};", r.code));
        // At this point the operand stack top is exactly the block's non-ref
        // results (the ref has been popped), so `branch` carries them on the null
        // path and leaves them for fall-through.
        self.branch(
            depth,
            Some(Val {
                code: format!("i32::from(matches!({temp}, GcRef::Null))"),
                ty: ValType::I32,
                stable: true,
            }),
        )?;
        // Fall-through continues with the ref (non-null) back on the stack.
        self.push(Val {
            code: temp,
            ty,
            stable: true,
        });
        Ok(())
    }

    /// `br_on_non_null $l`: branch to `$l` when the popped ref is non-null,
    /// carrying it (as non-null) plus the block's other values; else fall through
    /// with the ref dropped.
    pub(super) fn br_on_non_null(&mut self, depth: u32) -> Result<(), TranspileError> {
        let r = self.pop()?;
        let ty = r.ty;
        let temp = self.fresh_temp();
        self.line(format!("let {temp}: GcRef = {};", r.code));
        // Push the ref back as the stack-top carried value so `branch` carries it
        // on the (non-null) taken path.
        self.push(Val {
            code: temp.clone(),
            ty,
            stable: true,
        });
        self.branch(
            depth,
            Some(Val {
                code: format!("i32::from(!matches!({temp}, GcRef::Null))"),
                ty: ValType::I32,
                stable: true,
            }),
        )?;
        // `branch` (br_if-style) left the ref on the stack for fall-through, but
        // `br_on_non_null` drops it on the null path, so remove it here.
        self.pop()?;
        Ok(())
    }
}

/// Apply a packed field's sign/zero extension to a plain `i32` read, when the
/// operator requested it (`*.get_s`/`*.get_u`). A full-width read passes through
/// unchanged. Returns the (possibly wrapped) expression and its value type.
fn extend_packed(
    shape: &SlotShape,
    read: String,
    ty: ValType,
    ext: Option<bool>,
) -> (String, ValType) {
    match (shape, ext) {
        (SlotShape::Packed { shift, .. }, Some(true)) => {
            (format!("((({read}) << {shift}) >> {shift})"), ValType::I32)
        }
        (SlotShape::Packed { mask, .. }, Some(false)) => {
            (format!("(({read}) & {mask:#X})"), ValType::I32)
        }
        _ => (read, ty),
    }
}
