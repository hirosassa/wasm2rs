use wasmparser::{ConstExpr, Operator};

use super::{
    CompositeKind, ImportedGlobalInfo, abstract_is_gc, concrete_is_gc, i32_literal, i64_literal,
};
use crate::TranspileError;

/// An integer value on a constant-expression evaluation stack.
#[derive(Clone, Copy)]
pub(crate) enum ConstInt {
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

/// The globals a constant expression may read with `global.get`: the integer
/// value of each earlier immutable *defined* global (`None` for any other), after
/// the `imported` imported ones (whose values only the host knows).
#[derive(Clone, Copy)]
pub(crate) struct IntGlobals<'a> {
    pub imported: usize,
    pub values: &'a [Option<ConstInt>],
}

impl IntGlobals<'_> {
    fn get(&self, index: u32) -> Option<ConstInt> {
        let slot = usize::try_from(index).ok()?.checked_sub(self.imported)?;
        self.values.get(slot).copied().flatten()
    }
}

/// The integer value of a defined global's initializer, if it is one.
pub(crate) fn const_int_value(expr: &ConstExpr<'_>, globals: IntGlobals<'_>) -> Option<ConstInt> {
    eval_int_const_expr(expr, "global", globals).ok()
}

/// Evaluate an integer constant expression (`i32.const`/`i64.const` with the
/// extended-const `add`, `sub` and `mul`, which wrap like the runtime operators)
/// to its final value. `what` names the use for error messages.
fn eval_int_const_expr(
    expr: &ConstExpr<'_>,
    what: &str,
    globals: IntGlobals<'_>,
) -> Result<ConstInt, TranspileError> {
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
            Operator::GlobalGet { global_index } => {
                let value = globals.get(global_index).ok_or_else(|| {
                    TranspileError::Unsupported(format!(
                        "{what}: global.get of an imported, mutable or non-integer global"
                    ))
                })?;
                stack.push(value);
            }
            Operator::End => {}
            other => return Err(TranspileError::Unsupported(format!("{what}: {other:?}"))),
        }
    }
    single_result(stack, what)
}

/// The one value a well-formed constant expression leaves on the stack. Validation
/// guarantees exactly one, so this is defence in depth: it reports a bug in the
/// evaluator (or a caller that skipped validation) instead of silently keeping the
/// last value, and avoids a panicking `pop().unwrap()`.
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
pub(crate) fn const_expr_u32(
    expr: &ConstExpr<'_>,
    globals: IntGlobals<'_>,
) -> Result<u32, TranspileError> {
    match eval_int_const_expr(expr, "element offset", globals)? {
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
pub(crate) fn const_expr_u64(
    expr: &ConstExpr<'_>,
    globals: IntGlobals<'_>,
) -> Result<u64, TranspileError> {
    const_expr_u64_for(expr, "data offset", globals)
}

/// [`const_expr_u64`] with the offset's role (`what`) named in diagnostics, e.g.
/// the element segment of a 64-bit table.
pub(crate) fn const_expr_u64_for(
    expr: &ConstExpr<'_>,
    what: &str,
    globals: IntGlobals<'_>,
) -> Result<u64, TranspileError> {
    Ok(eval_int_const_expr(expr, what, globals)?.widen())
}

/// Translate a global's constant initializer expression to a Rust expression.
/// `type_kinds` classifies concrete heap types so a `ref.null` picks the null
/// value matching the reference's lowering. `global.get` reads an earlier
/// immutable *defined* global, whose initializer is inlined, or an immutable
/// *imported* one (they precede the defined ones in the index space), which is
/// read from the host (`imports`, the constructor's parameter) when the instance
/// is built.
pub(crate) fn const_expr_to_rust(
    expr: &ConstExpr<'_>,
    type_kinds: &[CompositeKind],
    imported: &[ImportedGlobalInfo],
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
                let index = usize::try_from(global_index).ok();
                // Only an immutable global is a constant. Validation already rejects
                // anything else; the `None` arms keep this lookup panic-free.
                let constant = match index {
                    Some(i) if i < imported.len() => imported
                        .get(i)
                        .filter(|g| !g.mutable)
                        .map(|_| format!("imports.get_global{i}()")),
                    _ => index
                        .and_then(|i| i.checked_sub(imported.len()))
                        .and_then(|i| defined.get(i))
                        .filter(|g| !g.mutable)
                        .map(|g| format!("({})", g.init)),
                };
                stack.push(constant.ok_or_else(|| {
                    TranspileError::Unsupported(
                        "global.get of a mutable or later global in a constant expression".into(),
                    )
                })?);
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

#[cfg(test)]
mod tests {
    use wasmparser::{BinaryReader, ValType};

    use super::*;
    use crate::codegen::GlobalInfo;

    // Constant expressions are built from raw bytes, bypassing validation, so the
    // evaluator's own defences (type mismatch, underflow, leftovers) are reachable.
    const I32_NEG1: [u8; 2] = [0x41, 0x7f];
    const I32_1: [u8; 2] = [0x41, 0x01];
    const I32_2: [u8; 2] = [0x41, 0x02];
    const I64_NEG1: [u8; 2] = [0x42, 0x7f];
    const I64_1: [u8; 2] = [0x42, 0x01];
    const I32_ADD: u8 = 0x6a;
    const I32_SUB: u8 = 0x6b;
    const I32_MUL: u8 = 0x6c;
    const I64_ADD: u8 = 0x7c;
    const DROP: u8 = 0x1a;
    const GLOBAL_GET: u8 = 0x23;
    const END: u8 = 0x0b;

    fn expr(parts: &[&[u8]]) -> Vec<u8> {
        let mut bytes: Vec<u8> = parts.concat();
        bytes.push(END);
        bytes
    }

    fn parse(bytes: &[u8]) -> ConstExpr<'_> {
        ConstExpr::new(BinaryReader::new(bytes, 0))
    }

    fn no_globals() -> IntGlobals<'static> {
        IntGlobals {
            imported: 0,
            values: &[],
        }
    }

    fn u32_of(bytes: &[u8]) -> Result<u32, TranspileError> {
        const_expr_u32(&parse(bytes), no_globals())
    }

    fn u64_of(bytes: &[u8]) -> Result<u64, TranspileError> {
        const_expr_u64(&parse(bytes), no_globals())
    }

    #[allow(
        clippy::panic,
        reason = "a test helper: a wrong variant fails the test"
    )]
    fn unsupported_msg(err: TranspileError) -> String {
        match err {
            TranspileError::Unsupported(m) => m,
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    #[test]
    fn i32_operators_wrap_at_the_u32_boundary() {
        // -1 as u32 is u32::MAX, so MAX + 1 wraps to 0.
        assert_eq!(u32_of(&expr(&[&I32_NEG1, &I32_1, &[I32_ADD]])).unwrap(), 0);
        // 0 - 1 wraps to u32::MAX; MAX * 2 wraps to MAX - 1.
        assert_eq!(
            u32_of(&expr(&[&[0x41, 0x00], &I32_1, &[I32_SUB]])).unwrap(),
            u32::MAX
        );
        assert_eq!(
            u32_of(&expr(&[&I32_NEG1, &I32_2, &[I32_MUL]])).unwrap(),
            u32::MAX - 1
        );
    }

    #[test]
    fn a_negative_i32_offset_widens_to_a_32_bit_address_not_a_sign_extended_one() {
        assert_eq!(u64_of(&expr(&[&I32_NEG1])).unwrap(), u64::from(u32::MAX));
    }

    #[test]
    fn i64_operators_wrap_at_the_u64_boundary() {
        assert_eq!(u64_of(&expr(&[&I64_NEG1, &I64_1, &[I64_ADD]])).unwrap(), 0);
    }

    #[test]
    fn an_i64_element_offset_is_rejected_for_a_32_bit_table() {
        let msg = unsupported_msg(u32_of(&expr(&[&I64_1])).unwrap_err());
        assert!(msg.contains("an i64 offset"), "{msg}");
    }

    #[test]
    fn mixing_i32_and_i64_operands_is_an_operand_type_mismatch() {
        let msg = unsupported_msg(u32_of(&expr(&[&I32_1, &I64_1, &[I32_ADD]])).unwrap_err());
        assert!(msg.contains("operand type mismatch"), "{msg}");
        assert!(msg.contains("two i32 operands"), "{msg}");
        let msg = unsupported_msg(u64_of(&expr(&[&I64_1, &I32_1, &[I64_ADD]])).unwrap_err());
        assert!(msg.contains("two i64 operands"), "{msg}");
    }

    #[test]
    fn a_binary_operator_without_two_operands_underflows() {
        assert!(matches!(
            u32_of(&expr(&[&I32_1, &[I32_ADD]])),
            Err(TranspileError::StackUnderflow)
        ));
        assert!(matches!(
            u64_of(&expr(&[&[I64_ADD]])),
            Err(TranspileError::StackUnderflow)
        ));
    }

    #[test]
    fn an_expression_must_leave_exactly_one_value() {
        let msg = unsupported_msg(u32_of(&expr(&[])).unwrap_err());
        assert!(msg.starts_with("empty"), "{msg}");
        let msg = unsupported_msg(u32_of(&expr(&[&I32_1, &I32_2])).unwrap_err());
        assert!(msg.contains("leftover operand"), "{msg}");
    }

    #[test]
    fn an_unsupported_operator_in_an_offset_is_named_with_its_role() {
        let msg = unsupported_msg(u32_of(&expr(&[&I32_1, &[DROP]])).unwrap_err());
        assert!(msg.starts_with("element offset"), "{msg}");
        assert!(msg.contains("Drop"), "{msg}");
    }

    #[test]
    fn global_get_in_an_offset_reads_only_known_defined_integer_globals() {
        let values = [Some(ConstInt::I32(7)), None];
        let globals = IntGlobals {
            imported: 1,
            values: &values,
        };
        let eval = |index: u8| {
            let bytes = expr(&[&[GLOBAL_GET, index]]);
            const_expr_u64(&parse(&bytes), globals)
        };
        // Index 0 is the imported global, 1 the first defined one, 2 the
        // defined one without a known value.
        assert!(eval(0).is_err());
        assert_eq!(eval(1).unwrap(), 7);
        assert!(eval(2).is_err());
        assert!(eval(3).is_err());
    }

    fn to_rust(
        bytes: &[u8],
        imported: &[ImportedGlobalInfo],
        defined: &[GlobalInfo],
    ) -> Result<String, TranspileError> {
        const_expr_to_rust(&parse(bytes), &[], imported, defined)
    }

    #[test]
    fn extended_const_arithmetic_lowers_to_wrapping_methods_in_operand_order() {
        assert_eq!(
            to_rust(&expr(&[&I32_1, &I32_2, &[I32_SUB]]), &[], &[]).unwrap(),
            "(1i32).wrapping_sub(2i32)"
        );
    }

    #[test]
    fn a_global_initializer_binary_operator_without_operands_underflows() {
        assert!(matches!(
            to_rust(&expr(&[&I32_1, &[I32_ADD]]), &[], &[]),
            Err(TranspileError::StackUnderflow)
        ));
    }

    #[test]
    fn a_global_initializer_rejects_unsupported_operators_and_stray_operands() {
        let msg = unsupported_msg(to_rust(&expr(&[&I32_1, &[DROP]]), &[], &[]).unwrap_err());
        assert!(msg.starts_with("global initializer"), "{msg}");
        let msg = unsupported_msg(to_rust(&expr(&[&I32_1, &I32_2]), &[], &[]).unwrap_err());
        assert!(msg.contains("leftover operand"), "{msg}");
    }

    #[test]
    fn global_get_in_an_initializer_inlines_immutable_defined_and_reads_imported_ones() {
        let imported = [
            ImportedGlobalInfo {
                ty: ValType::I32,
                mutable: false,
            },
            ImportedGlobalInfo {
                ty: ValType::I32,
                mutable: true,
            },
        ];
        let defined = [
            GlobalInfo {
                ty: ValType::I32,
                mutable: false,
                init: "5i32".into(),
            },
            GlobalInfo {
                ty: ValType::I32,
                mutable: true,
                init: "6i32".into(),
            },
        ];
        let get = |index: u8| to_rust(&expr(&[&[GLOBAL_GET, index]]), &imported, &defined);
        assert_eq!(get(0).unwrap(), "imports.get_global0()");
        assert_eq!(get(2).unwrap(), "(5i32)");
        // A mutable global (imported or defined) and an out-of-range index are
        // not constants.
        for index in [1, 3, 4] {
            let msg = unsupported_msg(get(index).unwrap_err());
            assert!(msg.contains("mutable or later global"), "{index}: {msg}");
        }
    }
}
