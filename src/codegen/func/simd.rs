//! SIMD/v128 lowering. A v128 is a `u128` whose bits are the little-endian
//! bytes of the vector, so lane access shifts/masks the `u128` and reinterprets
//! the relevant slice as the lane type. Whole-register bitwise operations map
//! straight to `u128` operators. Splat helpers live with the other pure runtime
//! free functions (see [`super::super::Rt`]); everything here is inline.

use wasmparser::ValType;

use super::super::Val;
use crate::TranspileError;

impl<'a> super::FuncGen<'a> {
    /// Extract one lane as a scalar. The lane's bits are shifted down to the low
    /// end of the `u128`; `build` reinterprets them (truncating via `as` to the
    /// lane width) into `result_ty`.
    pub(super) fn extract_lane(
        &mut self,
        lane: u8,
        lane_bits: u32,
        result_ty: ValType,
        build: impl FnOnce(&str) -> String,
    ) -> Result<(), TranspileError> {
        let v = self.pop()?;
        let shifted = format!("({} >> {})", v.code, lane as u32 * lane_bits);
        self.push_combined(build(&shifted), result_ty, v.stable)
    }

    /// Replace one lane with a scalar: clear the lane's bits with a shifted mask,
    /// then OR in the scalar (reinterpreted to the lane width) shifted into
    /// place. `mask` is the lane-width all-ones `u128` literal and `to_bits`
    /// renders the scalar as the lane's unsigned `u128` bit pattern.
    pub(super) fn replace_lane(
        &mut self,
        lane: u8,
        lane_bits: u32,
        mask: &str,
        to_bits: impl FnOnce(&str) -> String,
    ) -> Result<(), TranspileError> {
        let x = self.pop()?;
        let v = self.pop()?;
        let shift = lane as u32 * lane_bits;
        let code = format!(
            "(({} & !({mask} << {shift})) | (({}) << {shift}))",
            v.code,
            to_bits(&x.code),
        );
        self.push_combined(code, ValType::V128, v.stable && x.stable)
    }

    /// `v128.not`: bitwise complement of the whole register.
    pub(super) fn v128_not(&mut self) -> Result<(), TranspileError> {
        let v = self.pop()?;
        self.push_combined(format!("(!{})", v.code), ValType::V128, v.stable)
    }

    /// `v128.andnot`: `a & !b` over the whole register.
    pub(super) fn v128_andnot(&mut self) -> Result<(), TranspileError> {
        let b = self.pop()?;
        let a = self.pop()?;
        self.push_combined(
            format!("({} & !{})", a.code, b.code),
            ValType::V128,
            a.stable && b.stable,
        )
    }

    /// `v128.bitselect`: `(v1 & c) | (v2 & !c)`. `c` is read twice, so it is
    /// bound to a temporary to evaluate it exactly once.
    pub(super) fn v128_bitselect(&mut self) -> Result<(), TranspileError> {
        let c = self.pop()?;
        let v2 = self.pop()?;
        let v1 = self.pop()?;
        let ct = self.fresh_temp();
        self.line(format!("let {ct}: u128 = {};", c.code));
        self.push_combined(
            format!("(({} & {ct}) | ({} & !{ct}))", v1.code, v2.code),
            ValType::V128,
            v1.stable && v2.stable && c.stable,
        )
    }

    /// `v128.any_true`: 1 if any bit of the register is set, else 0.
    pub(super) fn v128_any_true(&mut self) -> Result<(), TranspileError> {
        let v = self.pop()?;
        self.push_combined(
            format!("i32::from({} != 0)", v.code),
            ValType::I32,
            v.stable,
        )
    }

    /// A binary lane-wise op `name(a, b) -> u128` (see `simd_rt.rs`): pop two
    /// v128 operands and push the helper call. Pure, so stable when both are.
    pub(super) fn call_simd_binop(&mut self, name: &'static str) -> Result<(), TranspileError> {
        let b = self.pop()?;
        let a = self.pop()?;
        self.used_simd.insert(name);
        self.push_combined(
            format!("{name}({}, {})", a.code, b.code),
            ValType::V128,
            a.stable && b.stable,
        )
    }

    /// A unary lane-wise op `name(a) -> u128` (see `simd_rt.rs`).
    pub(super) fn call_simd_unop(&mut self, name: &'static str) -> Result<(), TranspileError> {
        let a = self.pop()?;
        self.used_simd.insert(name);
        self.push_combined(format!("{name}({})", a.code), ValType::V128, a.stable)
    }

    /// Push a `v128.const` literal from its 16 little-endian bytes.
    pub(super) fn v128_const(&mut self, bytes: &[u8; 16]) {
        self.push(Val {
            code: format!("{}u128", u128::from_le_bytes(*bytes)),
            ty: ValType::V128,
            stable: true,
        });
    }
}
