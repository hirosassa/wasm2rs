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

    /// `i8x16.shuffle`: a binary op with a constant 16-byte index vector baked in
    /// as a third argument (see `i8x16_shuffle` in `simd_rt.rs`).
    pub(super) fn call_simd_shuffle(&mut self, lanes: [u8; 16]) -> Result<(), TranspileError> {
        let b = self.pop()?;
        let a = self.pop()?;
        self.used_simd.insert("i8x16_shuffle");
        let idx = u128::from_le_bytes(lanes);
        self.push_combined(
            format!("i8x16_shuffle({}, {}, {idx}u128)", a.code, b.code),
            ValType::V128,
            a.stable && b.stable,
        )
    }

    /// A lane-reducing op `name(a: u128) -> i32` (see `simd_rt.rs`): pop the
    /// v128 operand and push the scalar result. Used by `all_true`/`bitmask`.
    pub(super) fn call_simd_reduce(&mut self, name: &'static str) -> Result<(), TranspileError> {
        let a = self.pop()?;
        self.used_simd.insert(name);
        self.push_combined(format!("{name}({})", a.code), ValType::I32, a.stable)
    }

    /// A lane shift `name(a: u128, s: i32) -> u128` (see `simd_rt.rs`): pop the
    /// i32 shift count then the v128 operand.
    pub(super) fn call_simd_shift(&mut self, name: &'static str) -> Result<(), TranspileError> {
        let s = self.pop()?;
        let v = self.pop()?;
        self.used_simd.insert(name);
        self.push_combined(
            format!("{name}({}, {})", v.code, s.code),
            ValType::V128,
            v.stable && s.stable,
        )
    }

    /// A whole-register bitwise op against a constant `mask`, used for float
    /// `neg` (`^` the lane sign bits) and `abs` (`&` the magnitude bits). These
    /// are exact sign-bit rewrites, so unlike the arithmetic lane helpers they
    /// need no per-lane loop and stay bit-exact even for NaN.
    pub(super) fn v128_mask_op(&mut self, op: char, mask: u128) -> Result<(), TranspileError> {
        let v = self.pop()?;
        self.push_combined(
            format!("({} {op} 0x{mask:032x}u128)", v.code),
            ValType::V128,
            v.stable,
        )
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
