//! GC phase 4b: heap-allocated `struct` and `array` object operators.
//!
//! Every managed object is a `GcRef::Obj(Rc<RefCell<Vec<GcSlot>>>)` (see the
//! module-scope `GCREF_DEF`): a struct's slots are its fields in declaration
//! order, an array's slots are its elements. `struct.new`/`array.new` allocate;
//! the `get`/`set`/`len` operators dereference (trapping on a null handle via
//! `GcRef::obj()`), reading or writing a slot by index. Packed (`i8`/`i16`)
//! fields store their masked low bits in a `GcSlot::I32` and sign/zero-extend on
//! `*.get_s`/`*.get_u`.

use wasmparser::{PackedIndex, StorageType, ValType};

use super::super::{CompositeKind, FieldInfo, Val};
use crate::TranspileError;

/// Which slot variant backs a storage type, and how to wrap/unwrap it.
enum SlotShape {
    /// A packed field (`i8`/`i16`): stored masked in a `GcSlot::I32`. Carries the
    /// low-bit mask and the extension shift amount for signed reads.
    Packed { mask: u32, shift: u32 },
    /// A full value type stored in its matching slot variant.
    Val(ValType),
}

impl SlotShape {
    fn of(storage: StorageType) -> Result<Self, TranspileError> {
        Ok(match storage {
            StorageType::I8 => SlotShape::Packed {
                mask: 0xFF,
                shift: 24,
            },
            StorageType::I16 => SlotShape::Packed {
                mask: 0xFFFF,
                shift: 16,
            },
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

    /// The value type the plain (`.get`, no packed extension) read yields.
    fn result_ty(&self) -> ValType {
        match self {
            SlotShape::Packed { .. } => ValType::I32,
            SlotShape::Val(ty) => *ty,
        }
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
            .map(|f| SlotShape::of(f.storage))
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
        let shape = {
            let fields = self.struct_fields(type_index)?;
            let field = fields.get(field_index as usize).ok_or_else(|| {
                TranspileError::Unsupported(format!("struct field {field_index} out of range"))
            })?;
            SlotShape::of(field.storage)?
        };
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
        let shape = {
            let fields = self.struct_fields(type_index)?;
            let field = fields.get(field_index as usize).ok_or_else(|| {
                TranspileError::Unsupported(format!("struct field {field_index} out of range"))
            })?;
            SlotShape::of(field.storage)?
        };
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
        let shape = SlotShape::of(self.array_element(type_index)?.storage)?;
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
        let shape = SlotShape::of(self.array_element(type_index)?.storage)?;
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
        let shape = SlotShape::of(self.array_element(type_index)?.storage)?;
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

    /// A `match &<ref_expr> { ... }` yielding a Rust `bool`: whether the ref's
    /// runtime type is a subtype of the static target `T`. `null_matches` picks
    /// how the null handle is classified (a nullable target matches null).
    fn cast_test_bool(&self, ref_expr: &str, target: u32, null_matches: bool) -> String {
        let member = self.subtype_member("*ty", target);
        format!(
            "match &({ref_expr}) {{ GcRef::Null => {null_matches}, \
             GcRef::Obj {{ ty, .. }} => {member} }}"
        )
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
        let target = self.gc_type_index(hty).ok_or_else(|| {
            TranspileError::Unsupported("ref.test on a non-struct/array target".into())
        })?;
        let r = self.pop()?;
        let test = self.cast_test_bool(&r.code, target, null_matches);
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
        let target = self.gc_type_index(hty).ok_or_else(|| {
            TranspileError::Unsupported("ref.cast on a non-struct/array target".into())
        })?;
        let r = self.pop()?;
        let test = self.cast_test_bool("__r", target, null_matches);
        let code = format!(
            "{{ let __r = ({}); if {test} {{ __r }} else {{ panic!(\"ref.cast failed\") }} }}",
            r.code
        );
        self.materialize(code, Self::concrete_ref_ty(target)?)
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
        let target = self.gc_type_index(to_ref_type.heap_type()).ok_or_else(|| {
            TranspileError::Unsupported("br_on_cast to a non-struct/array target".into())
        })?;
        // Pin the ref into a stable temp so the branch carries it on the taken
        // path and leaves the same value on the stack for fall-through, without
        // re-evaluating (and so re-allocating) the operand.
        let r = self.pop()?;
        let temp = self.fresh_temp();
        self.line(format!("let {temp}: GcRef = {};", r.code));
        // The condition tests the runtime type against `to_ref_type` (whose
        // nullability decides how null is treated). `br_on_cast` branches on a
        // match; `br_on_cast_fail` branches on the complement.
        let matches = self.cast_test_bool(&temp, target, to_ref_type.is_nullable());
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
