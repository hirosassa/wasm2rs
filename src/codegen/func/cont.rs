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

use super::super::{ALLOW, CompositeKind, GenMeta, Node, TypeSig, Val, index_u32};
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
                "continuation body with parameters (phase 5b)".into(),
            ));
        }
        // Locals now live in the frame (loaded at entry, saved at every suspend),
        // so discard the default-init `let` bindings `FuncGen::new` seeded `cur`
        // with; the entry prologue below reloads them from `__frame` instead.
        self.cur.clear();

        // Each element is one `pc` state's arm body: the state's statements
        // followed by a terminal `StepResult` expression.
        let mut arms: Vec<String> = Vec::new();
        let mut pc: u32 = 0;
        // A pending tail cross-call checkpoint: a `call` to another step
        // function whose result becomes (part of) this function's result. The
        // arm it produces wraps its callee's `StepResult`, so it is only closed
        // out at `End`.
        let mut checkpoint: Option<u32> = None;
        for op in body.get_operators_reader()? {
            match op? {
                Operator::Suspend { tag_index } => {
                    if checkpoint.is_some() {
                        return Err(TranspileError::Unsupported(
                            "suspend after a cross-call checkpoint (phase 5: a checkpoint must be \
                             in tail position)"
                                .into(),
                        ));
                    }
                    let payload_tys = self
                        .ctx
                        .tags
                        .get(tag_index as usize)
                        .ok_or_else(|| {
                            TranspileError::Unsupported("suspend: unknown tag index".into())
                        })?
                        .clone();
                    let payload = self.encode_stack_tail(&payload_tys)?;
                    // Statements (e.g. `local.set`) computed since the last
                    // boundary run first; then the mutated locals are saved into
                    // the frame so the next resume reloads them, and the state
                    // advances before returning the suspension up.
                    let stmts = self.take_arm_statements()?;
                    let save = self.save_mutated_locals()?;
                    let next = pc + 1;
                    arms.push(format!(
                        "{stmts}{save}__frame.pc = {next}u32; StepResult::Suspend {{ \
                         tag: {tag_index}u32, payload: vec![{payload}] }}"
                    ));
                    pc = next;
                }
                Operator::Call { function_index }
                    if self.ctx.step_set.binary_search(&function_index).is_ok() =>
                {
                    self.begin_checkpoint(function_index, &mut checkpoint)?;
                }
                Operator::ReturnCall { function_index }
                    if self.ctx.step_set.binary_search(&function_index).is_ok() =>
                {
                    return Err(TranspileError::Unsupported(
                        "return_call across a continuation (phase 5)".into(),
                    ));
                }
                Operator::End => {
                    // No nested control flow is allowed, so this is the function
                    // end: the remaining stack is the function's results.
                    let results = self.results.clone();
                    let payload = self.encode_stack_tail(&results)?;
                    let arm = match checkpoint {
                        // The tail checkpoint resumes its callee once: on the
                        // callee's suspend, save this frame's locals and propagate
                        // the suspension up unchanged (this frame's `pc` is
                        // untouched, so the next resume re-enters here and drives
                        // the callee on); on the callee's return, its results
                        // (already on the operand stack as `__cret` reads) become
                        // this function's results. A checkpoint sits at a clean
                        // boundary, so no statements bracket it.
                        Some(g) => {
                            if !self.cur.is_empty() {
                                return Err(TranspileError::Unsupported(
                                    "statements after a cross-call checkpoint (phase 5b)".into(),
                                ));
                            }
                            let save = self.save_mutated_locals()?;
                            let bind = if payload.contains("__cret") {
                                "__cret"
                            } else {
                                "_"
                            };
                            format!(
                                "match self.cont_step_func{g}(&mut __frame.sub) {{ \
                                 StepResult::Suspend {{ tag: __t, payload: __p }} => {{ \
                                 {save}StepResult::Suspend {{ tag: __t, payload: __p }} }}, \
                                 StepResult::Return({bind}) => StepResult::Return(vec![{payload}]) }}"
                            )
                        }
                        None => {
                            let stmts = self.take_arm_statements()?;
                            format!("{stmts}StepResult::Return(vec![{payload}])")
                        }
                    };
                    arms.push(arm);
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
        // Locals live in `__frame`; a body may declare more than it reads, so the
        // same allowances the ordinary `func{N}`s carry keep the reload prologue
        // and per-suspend saves warning-free.
        src.push_str(line_prefix);
        src.push_str(ALLOW);
        src.push('\n');
        src.push_str(line_prefix);
        // `pub` so the root impl's `cont_step` can reach it when this body is
        // emitted into a separate chunk module (like the ordinary `func{N}`s).
        src.push_str(&format!(
            "pub fn cont_step_func{index}(&mut self, __frame: &mut ContFrame{index}) \
             -> StepResult {{\n"
        ));
        // Reload every local from the frame at entry; arm bodies reference `lN`
        // bare (unchanged from the ordinary lowering).
        for i in 0..self.local_types.len() {
            let keyword = if self.mutable_locals.contains(&index_u32(i)?) {
                "let mut"
            } else {
                "let"
            };
            src.push_str(line_prefix);
            src.push_str(&format!("    {keyword} l{i} = __frame.l{i};\n"));
        }
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

    /// Begin a tail cross-call checkpoint: a `call` to another step function
    /// `callee`. Like a suspend point, this requires a clean boundary — an empty
    /// operand stack (the callee takes no arguments in phase 5) and no pending
    /// statements — so that re-entering the arm on each callee-suspend re-runs
    /// nothing side-effecting before the resumed call. The callee's results are
    /// pushed as operands reading the `__cret` binding the arm introduces at
    /// `End`; only one such checkpoint is allowed per body.
    fn begin_checkpoint(
        &mut self,
        callee: u32,
        checkpoint: &mut Option<u32>,
    ) -> Result<(), TranspileError> {
        if checkpoint.is_some() {
            return Err(TranspileError::Unsupported(
                "more than one cross-call checkpoint in a continuation body (phase 5)".into(),
            ));
        }
        let (params, results) = self.ctx.full_sig(callee as usize).ok_or_else(|| {
            TranspileError::Unsupported("checkpoint call to unknown function".into())
        })?;
        if !params.is_empty() {
            return Err(TranspileError::Unsupported(
                "cross-call checkpoint to a continuation with parameters (phase 5)".into(),
            ));
        }
        let results = results.to_vec();
        if !self.stack.is_empty() || !self.cur.is_empty() {
            return Err(TranspileError::Unsupported(
                "non-empty operand stack before a cross-call checkpoint (phase 5)".into(),
            ));
        }
        for (i, ty) in results.iter().enumerate() {
            let code = decode_from_i64(&format!("__cret[{i}]"), *ty)?;
            self.push(Val {
                code,
                ty: *ty,
                stable: true,
            });
        }
        *checkpoint = Some(callee);
        Ok(())
    }

    /// Pop the top `tys.len()` operands (the tail matching `tys`, deepest first)
    /// and return them comma-joined as `i64`-encoded expressions. Requires the
    /// operand stack to be otherwise empty — a suspend/return point consumes the
    /// whole stack as its payload, so nothing survives the boundary. Pending
    /// statements (e.g. `local.set`) are flushed separately by the caller.
    fn encode_stack_tail(&mut self, tys: &[ValType]) -> Result<String, TranspileError> {
        let mut vals = Vec::with_capacity(tys.len());
        for _ in tys {
            vals.push(self.pop()?);
        }
        vals.reverse();
        if !self.stack.is_empty() {
            return Err(TranspileError::Unsupported(
                "non-empty operand stack at a continuation suspend/return (phase 5b)".into(),
            ));
        }
        let mut encoded = Vec::with_capacity(tys.len());
        for (val, ty) in vals.iter().zip(tys) {
            encoded.push(encode_to_i64(&val.code, *ty)?);
        }
        Ok(encoded.join(", "))
    }

    /// Drain the statements queued for the current `pc` state (from `local.set`,
    /// operand spills, and the like) as an inline, space-separated string. Phase
    /// 5b has no nested control flow in a continuation body, so every queued node
    /// is a straight-line statement; anything else is rejected.
    fn take_arm_statements(&mut self) -> Result<String, TranspileError> {
        let mut out = String::new();
        for node in std::mem::take(&mut self.cur) {
            // Only straight-line statements reach here in phase 5b; a
            // control-flow node (from a nested region or a `return`/branch) is
            // already rejected upstream, so this is a defensive guard.
            let Node::Line(text) = node else {
                return Err(TranspileError::Unsupported(
                    "unsupported control flow in a continuation body (phase 5b)".into(),
                ));
            };
            out.push_str(&text);
            out.push(' ');
        }
        Ok(out)
    }

    /// Save the mutated locals back into the frame, so the next resume reloads
    /// their current values. Unmutated locals never change from their frame
    /// default, so they need no write-back.
    fn save_mutated_locals(&self) -> Result<String, TranspileError> {
        let mut out = String::new();
        for i in 0..self.local_types.len() {
            if self.mutable_locals.contains(&index_u32(i)?) {
                out.push_str(&format!("__frame.l{i} = l{i}; "));
            }
        }
        Ok(out)
    }
}
