//! Typed-continuations (stack-switching) backend: emitting a continuation's
//! underlying function as a resumable *step* function.
//!
//! A function reachable as a continuation body (the target of a `ref.func` fed
//! into `cont.new`) is not emitted as an ordinary `func{N}`. Instead it becomes
//! `cont_step_func{N}(&mut self, __frame: &mut ContFrame{N}) -> StepResult`: a
//! little state machine keyed on `__frame.pc`. Each `suspend` splits the body
//! into another `pc` state — the arm saves the next `pc` into the frame and
//! returns `StepResult::Suspend`; falling off the end returns
//! `StepResult::Return`. Both the suspend payload and the returned results are
//! carried as `Vec<i64>` (the same width-erased encoding the exception path
//! uses), decoded back to their wasm types by the resumer.
//!
//! Phase 4 (the first observable generator) is deliberately narrow: a
//! continuation body must have no parameters, no locals and no nested control
//! flow, and its operand stack must be empty at every suspend point. Anything
//! else is rejected as `Unsupported` and revisited in a later phase.

use wasmparser::{FunctionBody, Operator, ValType};

use super::super::{CompositeKind, GenMeta, TypeSig};
use crate::TranspileError;

/// Encode a wasm value (given its Rust expression `code` and type) into the
/// `i64` slot used by `StepResult` payloads/results. Integers widen (sign- or
/// zero-preserving round-trips through [`decode_from_i64`]); floats carry their
/// bit pattern.
pub(super) fn encode_to_i64(code: &str, ty: ValType) -> Result<String, TranspileError> {
    Ok(match ty {
        ValType::I32 => format!("(({code}) as i64)"),
        ValType::I64 => format!("({code})"),
        ValType::F32 => format!("(f32::to_bits({code}) as i64)"),
        ValType::F64 => format!("(f64::to_bits({code}) as i64)"),
        other => {
            return Err(TranspileError::Unsupported(format!(
                "continuation value type {other:?} (only i32/i64/f32/f64)"
            )));
        }
    })
}

/// Decode an `i64` slot (given as the Rust expression `expr`) back into a wasm
/// value of type `ty`, inverting [`encode_to_i64`].
pub(super) fn decode_from_i64(expr: &str, ty: ValType) -> Result<String, TranspileError> {
    Ok(match ty {
        ValType::I32 => format!("(({expr}) as i32)"),
        ValType::I64 => format!("({expr})"),
        ValType::F32 => format!("f32::from_bits(({expr}) as u32)"),
        ValType::F64 => format!("f64::from_bits(({expr}) as u64)"),
        other => {
            return Err(TranspileError::Unsupported(format!(
                "continuation value type {other:?} (only i32/i64/f32/f64)"
            )));
        }
    })
}

/// The result types of the function type underlying a continuation type index.
pub(super) fn cont_result_types(
    type_kinds: &[CompositeKind],
    types: &[TypeSig],
    cont_type_index: u32,
) -> Result<Vec<ValType>, TranspileError> {
    let idx = usize::try_from(cont_type_index)
        .map_err(|_| TranspileError::Unsupported("continuation type index too large".into()))?;
    let CompositeKind::Cont(func_ty) = type_kinds.get(idx).ok_or_else(|| {
        TranspileError::Unsupported("continuation type index out of range".into())
    })?
    else {
        return Err(TranspileError::Unsupported(format!(
            "type index {cont_type_index} is not a continuation type"
        )));
    };
    let sig = types.get(*func_ty as usize).ok_or_else(|| {
        TranspileError::Unsupported("continuation underlying function type out of range".into())
    })?;
    Ok(sig.results.clone())
}

impl super::FuncGen<'_> {
    /// Emit this function as a resumable continuation step function (see the
    /// module docs). Consumes the generator; the operator stream is walked here
    /// rather than through [`run`](Self::run) so `suspend` can split the body
    /// into `pc` states.
    pub(in crate::codegen) fn emit_cont_step(
        mut self,
        index: usize,
        params: &[ValType],
        body: &FunctionBody<'_>,
        line_prefix: &str,
        out: &mut String,
    ) -> Result<GenMeta, TranspileError> {
        if !params.is_empty() {
            return Err(TranspileError::Unsupported(
                "continuation body with parameters (phase 4)".into(),
            ));
        }
        // `FuncGen::new` seeds `cur` with local declarations; a body with locals
        // would need them in the frame, which phase 4 does not model.
        if !self.local_types.is_empty() || !self.cur.is_empty() {
            return Err(TranspileError::Unsupported(
                "continuation body with locals (phase 4)".into(),
            ));
        }

        // Each element is one `pc` state's arm expression (a `StepResult`).
        let mut arms: Vec<String> = Vec::new();
        let mut pc: u32 = 0;
        for op in body.get_operators_reader()? {
            match op? {
                Operator::Suspend { tag_index } => {
                    let payload_tys = self
                        .ctx
                        .tags
                        .get(tag_index as usize)
                        .ok_or_else(|| {
                            TranspileError::Unsupported("suspend: unknown tag index".into())
                        })?
                        .clone();
                    let payload = self.encode_stack_tail(&payload_tys)?;
                    let next = pc + 1;
                    arms.push(format!(
                        "__frame.pc = {next}u32; StepResult::Suspend {{ tag: {tag_index}u32, \
                         payload: vec![{payload}] }}"
                    ));
                    pc = next;
                }
                Operator::End => {
                    // No nested control flow is allowed, so this is the function
                    // end: the remaining stack is the function's results.
                    let results = self.results.clone();
                    let payload = self.encode_stack_tail(&results)?;
                    arms.push(format!("StepResult::Return(vec![{payload}])"));
                    break;
                }
                other @ (Operator::Block { .. }
                | Operator::Loop { .. }
                | Operator::If { .. }
                | Operator::Else
                | Operator::Try { .. }
                | Operator::TryTable { .. }
                | Operator::Return
                | Operator::Br { .. }
                | Operator::BrIf { .. }
                | Operator::BrTable { .. }
                | Operator::Resume { .. }
                | Operator::Unreachable) => {
                    return Err(TranspileError::Unsupported(format!(
                        "operator {other:?} in a continuation body (phase 4)"
                    )));
                }
                other => self.emit_op(other)?,
            }
        }

        let mut src = String::new();
        src.push_str(line_prefix);
        // `pub` so the root impl's `cont_step` can reach it when this body is
        // emitted into a separate chunk module (like the ordinary `func{N}`s).
        src.push_str(&format!(
            "pub fn cont_step_func{index}(&mut self, __frame: &mut ContFrame{index}) \
             -> StepResult {{\n"
        ));
        src.push_str(line_prefix);
        src.push_str("    match __frame.pc {\n");
        for (state, arm) in arms.iter().enumerate() {
            src.push_str(line_prefix);
            src.push_str(&format!("        {state}u32 => {{ {arm} }}\n"));
        }
        src.push_str(line_prefix);
        src.push_str("        _ => unreachable!(),\n");
        src.push_str(line_prefix);
        src.push_str("    }\n");
        src.push_str(line_prefix);
        src.push_str("}\n");
        out.push_str(&src);

        Ok(GenMeta {
            helpers: self.used_helpers,
            rt: self.used_rt,
            simd: self.used_simd,
            dispatch_sigs: self.dispatch_sigs,
            uses_eh: self.uses_eh,
        })
    }

    /// Pop the top `tys.len()` operands (the tail matching `tys`, deepest first)
    /// and return them comma-joined as `i64`-encoded expressions. Requires the
    /// operand stack to be otherwise empty and no pending statements — phase 4
    /// only handles suspend/return points with a clean stack.
    fn encode_stack_tail(&mut self, tys: &[ValType]) -> Result<String, TranspileError> {
        let mut vals = Vec::with_capacity(tys.len());
        for _ in tys {
            vals.push(self.pop()?);
        }
        vals.reverse();
        if !self.stack.is_empty() {
            return Err(TranspileError::Unsupported(
                "non-empty operand stack at a continuation suspend/return (phase 4)".into(),
            ));
        }
        if !self.cur.is_empty() {
            return Err(TranspileError::Unsupported(
                "statements before a continuation suspend/return (phase 4)".into(),
            ));
        }
        let mut encoded = Vec::with_capacity(tys.len());
        for (val, ty) in vals.iter().zip(tys) {
            encoded.push(encode_to_i64(&val.code, *ty)?);
        }
        Ok(encoded.join(", "))
    }
}
