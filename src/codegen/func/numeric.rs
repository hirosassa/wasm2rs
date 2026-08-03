use wasmparser::ValType;

use super::super::{Rt, Val, rt_name, rust_type, unsigned_type};
use crate::TranspileError;

impl<'a> super::FuncGen<'a> {
    // ----- numeric operators -----------------------------------------------
    pub(super) fn binop_method(&mut self, method: &str) -> Result<(), TranspileError> {
        let rhs = self.pop()?;
        let lhs = self.pop()?;
        // Arithmetic/bitwise results keep the operand type (i32 or i64).
        self.push_combined(
            format!("{}.{method}({})", lhs.code, rhs.code),
            lhs.ty,
            lhs.stable && rhs.stable,
        )
    }

    pub(super) fn binop_infix(&mut self, op: &str) -> Result<(), TranspileError> {
        let rhs = self.pop()?;
        let lhs = self.pop()?;
        self.push_combined(
            format!("({} {op} {})", lhs.code, rhs.code),
            lhs.ty,
            lhs.stable && rhs.stable,
        )
    }

    pub(super) fn compare_zero(&mut self) -> Result<(), TranspileError> {
        let a = self.pop()?;
        self.push_combined(
            format!("i32::from({} == 0)", a.code),
            ValType::I32,
            a.stable,
        )
    }

    pub(super) fn compare_signed(&mut self, op: &str) -> Result<(), TranspileError> {
        let rhs = self.pop()?;
        let lhs = self.pop()?;
        self.push_combined(
            format!("i32::from({} {op} {})", lhs.code, rhs.code),
            ValType::I32,
            lhs.stable && rhs.stable,
        )
    }

    pub(super) fn compare_unsigned(&mut self, op: &str) -> Result<(), TranspileError> {
        let rhs = self.pop()?;
        let lhs = self.pop()?;
        // The operands are reinterpreted as the unsigned integer of their width.
        let unsigned = unsigned_type(lhs.ty)?;
        self.push_combined(
            format!(
                "i32::from(({} as {unsigned}) {op} ({} as {unsigned}))",
                lhs.code, rhs.code
            ),
            ValType::I32,
            lhs.stable && rhs.stable,
        )
    }

    /// A shift or rotate: `lhs.method(rhs as u32)`. `wrapping_shl`/`wrapping_shr`
    /// and `rotate_left`/`rotate_right` all take the count mod the bit width, so
    /// this matches wasm's masked shift/rotate count for both i32 and i64.
    pub(super) fn shift_op(&mut self, method: &str) -> Result<(), TranspileError> {
        let rhs = self.pop()?;
        let lhs = self.pop()?;
        self.push_combined(
            format!("{}.{method}({} as u32)", lhs.code, rhs.code),
            lhs.ty,
            lhs.stable && rhs.stable,
        )
    }

    /// Bind a possibly-trapping expression (integer div/rem) to a temporary at
    /// exactly this program point, so the trap fires in program order and is
    /// not lost if the value is later dropped or skipped by a branch. Pushes
    /// the resulting stable temporary. This mirrors how `call`/`memory_grow`
    /// materialise their side-effecting results.
    pub(super) fn materialize(&mut self, code: String, ty: ValType) -> Result<(), TranspileError> {
        let name = self.fresh_temp();
        self.line(format!("let {name}: {} = {code};", rust_type(ty)?));
        self.push(Val {
            code: name,
            ty,
            stable: true,
        });
        Ok(())
    }

    /// Signed division. Rust's `/` panics on both a zero divisor and
    /// `iN::MIN / -1`, matching the two wasm `div_s` traps.
    pub(super) fn div_signed(&mut self) -> Result<(), TranspileError> {
        let rhs = self.pop()?;
        let lhs = self.pop()?;
        self.materialize(format!("{} / {}", lhs.code, rhs.code), lhs.ty)
    }

    /// Signed remainder. `wrapping_rem` panics on a zero divisor and yields 0
    /// for `iN::MIN % -1`, matching wasm `rem_s`.
    pub(super) fn rem_signed(&mut self) -> Result<(), TranspileError> {
        let rhs = self.pop()?;
        let lhs = self.pop()?;
        self.materialize(format!("{}.wrapping_rem({})", lhs.code, rhs.code), lhs.ty)
    }

    /// Unsigned division (`op` = `/`) or remainder (`op` = `%`): reinterpret both
    /// operands as the unsigned integer of their width, apply `op` (which panics
    /// on a zero divisor), then reinterpret back to the signed type.
    pub(super) fn div_rem_unsigned(&mut self, op: &str) -> Result<(), TranspileError> {
        let rhs = self.pop()?;
        let lhs = self.pop()?;
        let unsigned = unsigned_type(lhs.ty)?;
        let signed = rust_type(lhs.ty)?;
        self.materialize(
            format!(
                "(({} as {unsigned}) {op} ({} as {unsigned})) as {signed}",
                lhs.code, rhs.code
            ),
            lhs.ty,
        )
    }

    /// A logical (unsigned) shift right: shift the unsigned reinterpretation so
    /// the high bits fill with zero, then reinterpret back to the signed type.
    /// Shifts do not trap, so the value stays a (stable) inline expression.
    pub(super) fn unsigned_shift(&mut self, method: &str) -> Result<(), TranspileError> {
        let rhs = self.pop()?;
        let lhs = self.pop()?;
        let unsigned = unsigned_type(lhs.ty)?;
        let signed = rust_type(lhs.ty)?;
        self.push_combined(
            format!(
                "(({} as {unsigned}).{method}({} as u32) as {signed})",
                lhs.code, rhs.code
            ),
            lhs.ty,
            lhs.stable && rhs.stable,
        )
    }

    /// A binary call to a free-function runtime helper `name(lhs, rhs)` (used
    /// for float `min`/`max`, whose wasm semantics differ from Rust's). The
    /// helpers are pure, so the result stays stable when both operands are.
    pub(super) fn call_rt_binop(&mut self, rt: Rt) -> Result<(), TranspileError> {
        let rhs = self.pop()?;
        let lhs = self.pop()?;
        self.used_rt.insert(rt);
        self.push_combined(
            format!("{}({}, {})", rt_name(rt), lhs.code, rhs.code),
            lhs.ty,
            lhs.stable && rhs.stable,
        )
    }

    /// A unary call to a possibly-trapping runtime helper (the non-saturating
    /// float->int truncations, which trap on NaN or an out-of-range operand).
    /// Like div/rem the result is materialised here so the trap fires in
    /// program order rather than being lost if the value is later dropped.
    pub(super) fn call_rt_unop_trapping(
        &mut self,
        rt: Rt,
        ty: ValType,
    ) -> Result<(), TranspileError> {
        let a = self.pop()?;
        self.used_rt.insert(rt);
        self.materialize(format!("{}({})", rt_name(rt), a.code), ty)
    }

    /// A unary method call `operand.method()` (float math like `abs`, `sqrt`).
    pub(super) fn unop_method(&mut self, method: &str) -> Result<(), TranspileError> {
        let a = self.pop()?;
        self.push_combined(format!("{}.{method}()", a.code), a.ty, a.stable)
    }

    /// wasm's bit-counting unary ops (`clz`/`ctz`/`popcnt`). Rust's
    /// `leading_zeros`/`trailing_zeros`/`count_ones` match wasm's semantics
    /// exactly — including `clz(0)` and `ctz(0)` returning the full width — but
    /// return `u32`, so the count is cast back to the operand's integer type.
    pub(super) fn bit_count(&mut self, ty: ValType, method: &str) -> Result<(), TranspileError> {
        let target = rust_type(ty)?;
        self.convert(ty, |x| format!("({x}.{method}() as {target})"))
    }

    /// Floating-point negation, parenthesised so it composes as a subexpression.
    pub(super) fn unop_neg(&mut self) -> Result<(), TranspileError> {
        let a = self.pop()?;
        self.push_combined(format!("(-{})", a.code), a.ty, a.stable)
    }

    /// A unary numeric conversion: pop one operand, build the converted
    /// expression from its code via `make`, and push it with `result_ty`. Used
    /// for wrap/extend, int<->float conversions, demote/promote, reinterpret
    /// and the saturating truncations — all pure and non-trapping.
    pub(super) fn convert(
        &mut self,
        result_ty: ValType,
        make: impl FnOnce(&str) -> String,
    ) -> Result<(), TranspileError> {
        let a = self.pop()?;
        self.push_combined(make(&a.code), result_ty, a.stable)
    }

    /// A single `operand as target` cast (`target` is the Rust primitive name),
    /// pushing the result as `result_ty`.
    pub(super) fn cast_as(
        &mut self,
        result_ty: ValType,
        target: &str,
    ) -> Result<(), TranspileError> {
        self.convert(result_ty, |x| format!("({x} as {target})"))
    }

    /// A cast that first reinterprets `operand as via` and then casts the result
    /// `as target`. Used for unsigned int<->float conversions and byte/half-word
    /// sign extension.
    pub(super) fn cast_through(
        &mut self,
        result_ty: ValType,
        via: &str,
        target: &str,
    ) -> Result<(), TranspileError> {
        self.convert(result_ty, |x| format!("(({x} as {via}) as {target})"))
    }

    /// A float->int reinterpret: read the operand's raw bits and cast them to the
    /// signed integer `target` of the same width.
    pub(super) fn reinterpret(
        &mut self,
        result_ty: ValType,
        target: &str,
    ) -> Result<(), TranspileError> {
        self.convert(result_ty, |x| format!("({x}.to_bits() as {target})"))
    }

    pub(super) fn select(&mut self) -> Result<(), TranspileError> {
        let cond = self.pop()?;
        let b = self.pop()?;
        let a = self.pop()?;
        // Parenthesised so the `if` expression composes safely when this value
        // is later embedded in a larger expression (e.g. as an operator arm).
        self.push_combined(
            format!(
                "(if {} != 0 {{ {} }} else {{ {} }})",
                cond.code, a.code, b.code
            ),
            a.ty,
            cond.stable && a.stable && b.stable,
        )
    }
}
