use wasmparser::{ConstExpr, Operator};

use super::{CompositeKind, abstract_is_gc, concrete_is_gc, i32_literal, i64_literal};
use crate::TranspileError;

/// Evaluate a constant offset expression (a single `i32.const`) to a `u32`,
/// as used for an active element segment's table offset.
pub(crate) fn const_expr_u32(expr: &ConstExpr<'_>) -> Result<u32, TranspileError> {
    let mut value: Option<u32> = None;
    for op in expr.get_operators_reader() {
        match op? {
            Operator::I32Const { value: v } => {
                // The offset is unsigned: a "negative" one is a huge index.
                value = Some(v.cast_unsigned());
            }
            Operator::End => {}
            other => {
                return Err(TranspileError::Unsupported(format!(
                    "element offset: {other:?}"
                )));
            }
        }
    }
    value.ok_or_else(|| TranspileError::Unsupported("empty element offset".into()))
}

/// Evaluate an active data segment's constant offset (an `i32.const`, or an
/// `i64.const` for a 64-bit memory) to a `u64`. An `i32` offset is unsigned, so a
/// "negative" one is a huge address, and the segment traps at instantiation
/// when it lies past the end of the memory. A negative `i64` offset is rejected: its sum with the length
/// would wrap `usize`.
pub(crate) fn const_expr_u64(expr: &ConstExpr<'_>) -> Result<u64, TranspileError> {
    let mut value: Option<u64> = None;
    for op in expr.get_operators_reader() {
        match op? {
            // An `i32` offset is unsigned: a "negative" one is a huge address.
            Operator::I32Const { value: v } => value = Some(u64::from(v.cast_unsigned())),
            Operator::I64Const { value: v } => {
                value = Some(
                    u64::try_from(v)
                        .map_err(|_| TranspileError::Unsupported("negative data offset".into()))?,
                );
            }
            Operator::End => {}
            other => {
                return Err(TranspileError::Unsupported(format!(
                    "data offset: {other:?}"
                )));
            }
        }
    }
    value.ok_or_else(|| TranspileError::Unsupported("empty data offset".into()))
}

/// Translate a global's constant initializer expression to a Rust expression.
/// `type_kinds` classifies concrete heap types so a `ref.null` picks the null
/// value matching the reference's lowering.
pub(crate) fn const_expr_to_rust(
    expr: &ConstExpr<'_>,
    type_kinds: &[CompositeKind],
) -> Result<String, TranspileError> {
    let mut value: Option<String> = None;
    for op in expr.get_operators_reader() {
        match op? {
            Operator::I32Const { value: v } => value = Some(i32_literal(v)),
            Operator::I64Const { value: v } => value = Some(i64_literal(v)),
            Operator::F32Const { value: v } => {
                value = Some(format!("f32::from_bits({}u32)", v.bits()));
            }
            Operator::F64Const { value: v } => {
                value = Some(format!("f64::from_bits({}u64)", v.bits()));
            }
            // A ref-typed initializer's null depends on the reference's Rust
            // lowering: a managed GC reference (`struct`/`array`/abstract GC)
            // is `GcRef::Null`, while a `funcref`/`externref`/`contref` handle
            // is the `u32::MAX` sentinel. `ref.func f` is the function's index.
            Operator::RefNull { hty } => {
                value = Some(if abstract_is_gc(hty) || concrete_is_gc(hty, type_kinds) {
                    "GcRef::Null".to_string()
                } else {
                    "u32::MAX".to_string()
                });
            }
            Operator::RefFunc { function_index } => {
                value = Some(format!("{function_index}u32"));
            }
            Operator::End => {}
            other => {
                return Err(TranspileError::Unsupported(format!(
                    "global initializer: {other:?}"
                )));
            }
        }
    }
    value.ok_or_else(|| TranspileError::Unsupported("empty global initializer".into()))
}
