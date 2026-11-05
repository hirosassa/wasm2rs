use wasmparser::{ConstExpr, Operator};

use super::{CompositeKind, abstract_is_gc, concrete_is_gc, i32_literal, i64_literal};
use crate::TranspileError;

/// An integer value on a constant-expression evaluation stack.
#[derive(Clone, Copy)]
enum ConstInt {
    I32(u32),
    I64(u64),
}

impl ConstInt {
    fn widen(self) -> u64 {
        match self {
            ConstInt::I32(v) => u64::from(v),
            ConstInt::I64(v) => v,
        }
    }
}

/// Evaluate an integer constant expression (`i32.const`/`i64.const` with the
/// extended-const `add`, `sub` and `mul`, which wrap like the runtime operators)
/// to its final value. `what` names the use for error messages.
fn eval_int_const_expr(expr: &ConstExpr<'_>, what: &str) -> Result<ConstInt, TranspileError> {
    let mut stack: Vec<ConstInt> = Vec::new();
    for op in expr.get_operators_reader() {
        match op? {
            // An offset is unsigned: a "negative" one is a huge address.
            Operator::I32Const { value: v } => stack.push(ConstInt::I32(v.cast_unsigned())),
            Operator::I64Const { value: v } => stack.push(ConstInt::I64(v.cast_unsigned())),
            Operator::I32Add => binary_i32(&mut stack, u32::wrapping_add)?,
            Operator::I32Sub => binary_i32(&mut stack, u32::wrapping_sub)?,
            Operator::I32Mul => binary_i32(&mut stack, u32::wrapping_mul)?,
            Operator::I64Add => binary_i64(&mut stack, u64::wrapping_add)?,
            Operator::I64Sub => binary_i64(&mut stack, u64::wrapping_sub)?,
            Operator::I64Mul => binary_i64(&mut stack, u64::wrapping_mul)?,
            Operator::End => {}
            other => return Err(TranspileError::Unsupported(format!("{what}: {other:?}"))),
        }
    }
    single_result(stack, what)
}

/// The one value a well-formed constant expression leaves on the stack. The
/// transpiler does not run the validator, so a malformed expression with none or
/// several values is rejected here instead of silently keeping the last one.
fn single_result<T>(mut stack: Vec<T>, what: &str) -> Result<T, TranspileError> {
    let value = stack
        .pop()
        .ok_or_else(|| TranspileError::Unsupported(format!("empty {what}")))?;
    if stack.is_empty() {
        Ok(value)
    } else {
        Err(TranspileError::Unsupported(format!(
            "{what}: leftover operand on the stack"
        )))
    }
}

fn operand_mismatch(expected: &str) -> TranspileError {
    TranspileError::Unsupported(format!(
        "constant expression: operand type mismatch (expected two {expected} operands)"
    ))
}

fn binary_i32(stack: &mut Vec<ConstInt>, f: fn(u32, u32) -> u32) -> Result<(), TranspileError> {
    match (stack.pop(), stack.pop()) {
        (Some(ConstInt::I32(b)), Some(ConstInt::I32(a))) => {
            stack.push(ConstInt::I32(f(a, b)));
            Ok(())
        }
        (Some(_), Some(_)) => Err(operand_mismatch("i32")),
        _ => Err(TranspileError::StackUnderflow),
    }
}

fn binary_i64(stack: &mut Vec<ConstInt>, f: fn(u64, u64) -> u64) -> Result<(), TranspileError> {
    match (stack.pop(), stack.pop()) {
        (Some(ConstInt::I64(b)), Some(ConstInt::I64(a))) => {
            stack.push(ConstInt::I64(f(a, b)));
            Ok(())
        }
        (Some(_), Some(_)) => Err(operand_mismatch("i64")),
        _ => Err(TranspileError::StackUnderflow),
    }
}

/// Evaluate a constant offset expression to a `u32`, as used for an active
/// element segment's table offset. The offset is unsigned, so a "negative" one is
/// a huge index.
pub(crate) fn const_expr_u32(expr: &ConstExpr<'_>) -> Result<u32, TranspileError> {
    match eval_int_const_expr(expr, "element offset")? {
        ConstInt::I32(v) => Ok(v),
        ConstInt::I64(_) => Err(TranspileError::Unsupported(
            "element offset: an i64 offset".into(),
        )),
    }
}

/// Evaluate an active data segment's constant offset (an `i32` expression, or an
/// `i64` one for a 64-bit memory) to a `u64`. The offset is unsigned, so a
/// "negative" one is a huge address, and the segment traps at instantiation
/// when it lies past the end of the memory.
pub(crate) fn const_expr_u64(expr: &ConstExpr<'_>) -> Result<u64, TranspileError> {
    const_expr_u64_for(expr, "data offset")
}

/// [`const_expr_u64`] with the offset's role (`what`) named in diagnostics, e.g.
/// the element segment of a 64-bit table.
pub(crate) fn const_expr_u64_for(expr: &ConstExpr<'_>, what: &str) -> Result<u64, TranspileError> {
    Ok(eval_int_const_expr(expr, what)?.widen())
}

/// Translate a global's constant initializer expression to a Rust expression.
/// `type_kinds` classifies concrete heap types so a `ref.null` picks the null
/// value matching the reference's lowering. `global.get` reads an earlier
/// immutable *defined* global: its initializer is inlined. `imported` is the
/// number of imported globals (they precede the defined ones in the index space,
/// and reading one needs the host, which is not supported here).
pub(crate) fn const_expr_to_rust(
    expr: &ConstExpr<'_>,
    type_kinds: &[CompositeKind],
    imported: usize,
    defined: &[crate::codegen::GlobalInfo],
) -> Result<String, TranspileError> {
    let mut stack: Vec<String> = Vec::new();
    for op in expr.get_operators_reader() {
        match op? {
            Operator::I32Const { value: v } => stack.push(i32_literal(v)),
            Operator::I64Const { value: v } => stack.push(i64_literal(v)),
            Operator::F32Const { value: v } => {
                stack.push(format!("f32::from_bits({}u32)", v.bits()));
            }
            Operator::F64Const { value: v } => {
                stack.push(format!("f64::from_bits({}u64)", v.bits()));
            }
            // A v128 is a `u128` of the vector's little-endian bytes.
            Operator::V128Const { value } => {
                stack.push(format!("{}u128", u128::from_le_bytes(*value.bytes())));
            }
            // The extended-const integer operators wrap, like the runtime ones.
            Operator::I32Add | Operator::I64Add => binary(&mut stack, "wrapping_add")?,
            Operator::I32Sub | Operator::I64Sub => binary(&mut stack, "wrapping_sub")?,
            Operator::I32Mul | Operator::I64Mul => binary(&mut stack, "wrapping_mul")?,
            // A ref-typed initializer's null depends on the reference's Rust
            // lowering: a managed GC reference (`struct`/`array`/abstract GC)
            // is `GcRef::Null`, while a `funcref`/`externref`/`contref` handle
            // is the `u32::MAX` sentinel. `ref.func f` is the function's index.
            Operator::RefNull { hty } => {
                stack.push(if abstract_is_gc(hty) || concrete_is_gc(hty, type_kinds) {
                    "GcRef::Null".to_string()
                } else {
                    "u32::MAX".to_string()
                });
            }
            Operator::RefFunc { function_index } => {
                stack.push(format!("{function_index}u32"));
            }
            Operator::GlobalGet { global_index } => {
                let source = usize::try_from(global_index)
                    .ok()
                    .and_then(|i| i.checked_sub(imported))
                    .and_then(|i| defined.get(i))
                    .ok_or_else(|| {
                        TranspileError::Unsupported(
                            "global.get of an imported (or later) global in a constant expression"
                                .into(),
                        )
                    })?;
                stack.push(format!("({})", source.init));
            }
            Operator::End => {}
            other => {
                return Err(TranspileError::Unsupported(format!(
                    "global initializer: {other:?}"
                )));
            }
        }
    }
    single_result(stack, "global initializer")
}

/// Combine the top two operands with the wrapping method `method`.
fn binary(stack: &mut Vec<String>, method: &str) -> Result<(), TranspileError> {
    let (Some(b), Some(a)) = (stack.pop(), stack.pop()) else {
        return Err(TranspileError::StackUnderflow);
    };
    stack.push(format!("({a}).{method}({b})"));
    Ok(())
}
