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
//! A continuation body may have locals (kept in the frame across suspends) and
//! numeric parameters (injected by the first `resume` and decoded into their
//! parameter locals at `pc == 0`). It may also contain nested structured
//! control flow (`block`/`loop`/`if` and the branches into them), provided no
//! `suspend` (nor a cross-call checkpoint) occurs *inside* a region — such a
//! region uses Rust's own control flow and renders as a single straight-line
//! chunk within one `pc` state. A `suspend` that crosses a region boundary
//! would have to weave the `pc` machine through the nested structure and is
//! rejected for now. The operand stack must still be empty at every suspend
//! point. Anything else is rejected as `Unsupported` and revisited later.

use wasmparser::{FunctionBody, HeapType, Operator, ValType};

use super::super::{
    ALLOW, CompositeKind, GenMeta, Node, TypeSig, Val, flatten_cont_body, index_u32,
    render_body_into, render_nodes_into, rust_type,
};
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

/// Validate that `cont_type_index` names a continuation type, without needing its
/// underlying signature (used where only the kind matters, e.g. `resume_throw`).
pub(super) fn require_cont_type(
    type_kinds: &[CompositeKind],
    cont_type_index: u32,
) -> Result<(), TranspileError> {
    let idx = usize::try_from(cont_type_index)
        .map_err(|_| TranspileError::Unsupported("continuation type index too large".into()))?;
    match type_kinds.get(idx) {
        Some(CompositeKind::Cont(_)) => Ok(()),
        _ => Err(TranspileError::Unsupported(format!(
            "type index {cont_type_index} is not a continuation type"
        ))),
    }
}

/// The result types of the function type underlying a continuation type index.
pub(super) fn cont_result_types(
    type_kinds: &[CompositeKind],
    types: &[TypeSig],
    cont_type_index: u32,
) -> Result<Vec<ValType>, TranspileError> {
    Ok(cont_underlying_sig(type_kinds, types, cont_type_index)?
        .results
        .clone())
}

/// The parameter types of the function type underlying a continuation type
/// index — the values a `resume` of that continuation injects (the body's
/// parameters at the first step; empty for a parameter-less body).
pub(super) fn cont_param_types(
    type_kinds: &[CompositeKind],
    types: &[TypeSig],
    cont_type_index: u32,
) -> Result<Vec<ValType>, TranspileError> {
    Ok(cont_underlying_sig(type_kinds, types, cont_type_index)?
        .params
        .clone())
}

/// The function signature underlying a continuation type index.
fn cont_underlying_sig<'a>(
    type_kinds: &[CompositeKind],
    types: &'a [TypeSig],
    cont_type_index: u32,
) -> Result<&'a TypeSig, TranspileError> {
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
    types.get(*func_ty as usize).ok_or_else(|| {
        TranspileError::Unsupported("continuation underlying function type out of range".into())
    })
}

/// The module type index a concrete reference `(ref $t)` points at (e.g. the
/// self-continuation type in a `switch` target's trailing parameter). Rejects
/// abstract or out-of-module references, which cannot name a continuation type.
fn concrete_ref_index(ty: ValType) -> Result<u32, TranspileError> {
    if let ValType::Ref(rt) = ty
        && let HeapType::Concrete(idx) = rt.heap_type()
        && let Some(module_idx) = idx.as_module_index()
    {
        return Ok(module_idx);
    }
    Err(TranspileError::Unsupported(
        "switch requires a concrete continuation reference".into(),
    ))
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
        // A body whose `suspend` or `switch` crosses a nested region cannot render
        // each region as one straight-line arm; it needs the `pc` state machine
        // woven through the nesting, which the flat path does by lowering the whole
        // body through the continuation flattener.
        if Self::transfer_crosses_region(body)? {
            return self.emit_cont_step_flat(index, params, body, line_prefix, out);
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
                    if !self.frames.is_empty() {
                        return Err(TranspileError::Unsupported(
                            "suspend inside nested control flow in a continuation body (phase \
                             5b-2b: a suspend crossing a region is not yet lowered)"
                                .into(),
                        ));
                    }
                    if checkpoint.is_some() {
                        return Err(TranspileError::Unsupported(
                            "suspend after a cross-call checkpoint (phase 5: a checkpoint must be \
                             in tail position)"
                                .into(),
                        ));
                    }
                    let payload = self.encode_suspend_payload(tag_index)?;
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
                    // `suspend $t : [p*] -> [r*]`: the results `r*` are the
                    // values the resuming side injects, delivered as the next
                    // step's `__args`. Push them as the resumed state's initial
                    // operands so the code after the suspend consumes them.
                    self.push_suspend_results(tag_index)?;
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
                Operator::End if !self.frames.is_empty() => {
                    // A nested region's end (`block`/`loop`/`if`): the ordinary
                    // lowering closes it into a `Node::Region` in `self.cur`,
                    // which `take_arm_statements` renders into the current arm as
                    // straight-line Rust control flow.
                    self.emit_op(Operator::End)?;
                }
                Operator::End => {
                    // Control that diverges before the outermost `end` (e.g. an
                    // infinite `loop`) makes any trailing `StepResult::Return`
                    // unreachable and leaves the operand stack unreadable, so —
                    // like `end_function` — emit only the (already diverging)
                    // statements as the arm. A checkpoint sits in reachable tail
                    // position, so it never coincides with an unreachable end.
                    if !self.reachable {
                        arms.push(self.take_arm_statements()?);
                        break;
                    }
                    // The outermost `end`: the remaining stack is the function's
                    // results.
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
                            // A checkpoint at `pc == 0` re-runs the header's param
                            // prologue on every re-entry, so a switch-back's
                            // injection (delivered as `__args`) would clobber this
                            // body's own parameter slots. Reject that combination
                            // rather than miscompile; a parameter-less outer (or one
                            // that suspends before the checkpoint, moving it past
                            // `pc == 0`) is unaffected.
                            if pc == 0 && !params.is_empty() {
                                return Err(TranspileError::Unsupported(
                                    "cross-call checkpoint at the entry of a continuation body \
                                     with parameters (phase 8: a switch-back would clobber them)"
                                        .into(),
                                ));
                            }
                            let save = self.save_mutated_locals()?;
                            let bind = if payload.contains("__cret") {
                                "__cret"
                            } else {
                                "_"
                            };
                            // Forward this step's `__args` to the callee: on the
                            // first drive they are ignored (the callee takes no
                            // parameters, so its `pc == 0` reads nothing); on a
                            // switch-back or suspend-resume the callee — parked at a
                            // transfer point below this frame — is the one that
                            // consumes the injected values.
                            format!(
                                "match self.cont_step_func{g}(&mut __frame.sub, __args) {{ \
                                 StepResult::Suspend {{ tag: __t, payload: __p }} => {{ \
                                 {save}StepResult::Suspend {{ tag: __t, payload: __p }} }}, \
                                 StepResult::Switch {{ tag: __t, target: __tgt, args: __a }} => {{ \
                                 {save}StepResult::Switch {{ tag: __t, target: __tgt, args: __a }} \
                                 }}, \
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
                Operator::Switch {
                    cont_type_index,
                    tag_index,
                } => {
                    if !self.frames.is_empty() {
                        return Err(TranspileError::Unsupported(
                            "switch inside nested control flow in a continuation body (phase \
                             8-2: a switch crossing a region is not yet lowered)"
                                .into(),
                        ));
                    }
                    if checkpoint.is_some() {
                        return Err(TranspileError::Unsupported(
                            "switch after a cross-call checkpoint (phase 8)".into(),
                        ));
                    }
                    let (payload_tys, injected_tys) =
                        self.switch_transfer_types(cont_type_index)?;
                    // Fix the evaluation order across the suspension boundary:
                    // freeze every operand into a temp so the target handle and the
                    // payload read stable names in the returned `Switch`.
                    self.spill_nonstable()?;
                    // The target continuation handle is on top; the payload `t1*`
                    // sits below it and is handed to that target.
                    let target = self.pop()?;
                    let payload = self.encode_stack_tail(&payload_tys)?;
                    let stmts = self.take_arm_statements()?;
                    let save = self.save_mutated_locals()?;
                    let next = pc + 1;
                    arms.push(format!(
                        "{stmts}{save}__frame.pc = {next}u32; StepResult::Switch {{ \
                         tag: {tag_index}u32, target: {}, args: vec![{payload}] }}",
                        target.code
                    ));
                    pc = next;
                    // Control switches back here carrying `t2*` — the
                    // self-continuation's parameters — as the next step's `__args`.
                    self.push_injected(&injected_tys)?;
                }
                other @ (Operator::Try { .. }
                | Operator::TryTable { .. }
                | Operator::Return
                | Operator::Resume { .. }
                | Operator::Unreachable) => {
                    return Err(TranspileError::Unsupported(format!(
                        "operator {other:?} in a continuation body (phase 5b-2b)"
                    )));
                }
                other => self.emit_op(other)?,
            }
        }

        let mut src = String::new();
        self.write_cont_step_header(index, params, line_prefix, &mut src)?;
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

    /// Whether any control-transfer point (`suspend` or `switch`) in `body` occurs
    /// inside a nested region (control depth > 0). Such a body cannot render each
    /// region as a single straight-line arm — the `pc` machine must thread through
    /// the nesting — so it takes the flat path ([`Self::emit_cont_step_flat`]). A
    /// body whose transfers are all at the top level keeps the simpler
    /// arm-splitting lowering.
    fn transfer_crosses_region(body: &FunctionBody<'_>) -> Result<bool, TranspileError> {
        let mut depth: usize = 0;
        for op in body.get_operators_reader()? {
            match op? {
                Operator::Block { .. }
                | Operator::Loop { .. }
                | Operator::If { .. }
                | Operator::Try { .. }
                | Operator::TryTable { .. } => depth += 1,
                Operator::End => depth = depth.saturating_sub(1),
                Operator::Suspend { .. } | Operator::Switch { .. } if depth > 0 => {
                    return Ok(true);
                }
                _ => {}
            }
        }
        Ok(false)
    }

    /// Emit this function as a resumable step function via the flat continuation
    /// dispatch (see the module docs and [`flatten_cont_body`]). Unlike
    /// [`Self::emit_cont_step`], the whole body is first walked into a [`Node`]
    /// tree (with each `suspend` recorded as a [`Node::Suspend`]), then lowered to
    /// a `pc` state machine that can be re-entered at a suspend inside a region.
    fn emit_cont_step_flat(
        mut self,
        index: usize,
        params: &[ValType],
        body: &FunctionBody<'_>,
        line_prefix: &str,
        out: &mut String,
    ) -> Result<GenMeta, TranspileError> {
        // Locals live in the frame (see `emit_cont_step`); drop the default-init
        // bindings and reload from `__frame` in the entry prologue instead.
        self.cur.clear();

        // Fall-off-the-end results, captured when the outermost `end` is reached
        // and the program point is still reachable (a diverging body never falls
        // through, so its exit stays unreachable).
        let mut return_payload: Option<String> = None;
        // A cross-call checkpoint in the flat lowering: at most one per body, so
        // the single nested `sub` frame is enough (matching `begin_checkpoint`).
        let mut checkpoint: Option<u32> = None;
        for op in body.get_operators_reader()? {
            match op? {
                Operator::Suspend { tag_index } => self.emit_suspend_node(tag_index)?,
                Operator::Switch {
                    cont_type_index,
                    tag_index,
                } => self.emit_switch_node(cont_type_index, tag_index)?,
                Operator::Call { function_index }
                    if self.ctx.step_set.binary_search(&function_index).is_ok() =>
                {
                    self.emit_checkpoint_node(function_index, &mut checkpoint)?;
                }
                Operator::ReturnCall { function_index }
                    if self.ctx.step_set.binary_search(&function_index).is_ok() =>
                {
                    return Err(TranspileError::Unsupported(
                        "return_call across a continuation (phase 5)".into(),
                    ));
                }
                // A nested region's `end` closes it into a `Node::Region` in
                // `self.cur` via the ordinary lowering.
                Operator::End if !self.frames.is_empty() => self.emit_op(Operator::End)?,
                // The outermost `end`: the remaining operands are the function's
                // results. Capture them as the fall-through return; the body tree
                // stays in `self.cur` for the flattener below.
                Operator::End => {
                    if self.reachable {
                        let results = self.results.clone();
                        return_payload = Some(self.encode_stack_tail(&results)?);
                    }
                }
                other @ (Operator::Try { .. }
                | Operator::TryTable { .. }
                | Operator::Return
                | Operator::Resume { .. }) => {
                    return Err(TranspileError::Unsupported(format!(
                        "operator {other:?} in a continuation body (phase 5b-2b)"
                    )));
                }
                other => self.emit_op(other)?,
            }
        }

        let exit_stmt = match return_payload {
            Some(payload) => format!("return StepResult::Return(vec![{payload}]);"),
            // A diverging body never falls off its end, so its exit is unreachable.
            None => "unreachable!();".to_string(),
        };
        let nodes = std::mem::take(&mut self.cur);
        let flat = flatten_cont_body(nodes, exit_stmt);

        let mut src = String::new();
        self.write_cont_step_header(index, params, line_prefix, &mut src)?;
        render_body_into(flat, line_prefix, &mut src);
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

    /// Record a `suspend` as a [`Node::Suspend`] in the current scope: pop and
    /// encode its payload, snapshot the mutated locals, and push the values the
    /// resumer will inject (`__args`) as the resumed state's initial operands.
    /// Shared shape with the top-level suspend handling in [`Self::emit_cont_step`],
    /// but deferred into the node tree so a suspend inside a region survives to the
    /// flattener.
    fn emit_suspend_node(&mut self, tag_index: u32) -> Result<(), TranspileError> {
        let payload_tys = self.suspend_payload_tys(tag_index)?;
        let payload = self.pop_encode_tail(&payload_tys)?;
        // Operands still on the stack below the payload (e.g. a region's entry
        // parameters) outlive the suspend. A suspend returns from the step
        // function, so anything kept only in a local `let` would be lost on the
        // next resume; save these survivors into the frame's `ostack` and rewrite
        // each to read back from there, so the resumed state (a later invocation)
        // sees the value that was live at the suspend.
        let save_ostack = self.save_surviving_operands()?;
        let save = format!("{save_ostack}{}", self.save_mutated_locals()?);
        self.cur.push(Node::Suspend {
            tag: tag_index,
            payload,
            save,
        });
        self.push_suspend_results(tag_index)
    }

    /// Record a `switch` as a [`Node::Switch`] in the current scope (flat lowering):
    /// pop the target continuation handle and encode the payload `t1*`, snapshot
    /// the operands surviving below (as [`Self::emit_suspend_node`] does), save the
    /// mutated locals, and push the self-continuation parameters `t2*` — delivered
    /// as the next step's `__args` when control switches back — as the resumed
    /// state's initial operands. The flat-path counterpart of the top-level switch
    /// handling in [`Self::emit_cont_step`].
    fn emit_switch_node(
        &mut self,
        cont_type_index: u32,
        tag_index: u32,
    ) -> Result<(), TranspileError> {
        let (payload_tys, injected_tys) = self.switch_transfer_types(cont_type_index)?;
        // The target continuation handle is on top; the payload `t1*` sits below it.
        let target = self.pop()?;
        let payload = self.pop_encode_tail(&payload_tys)?;
        let save_ostack = self.save_surviving_operands()?;
        let save = format!("{save_ostack}{}", self.save_mutated_locals()?);
        self.cur.push(Node::Switch {
            tag: tag_index,
            target: target.code,
            payload,
            save,
        });
        self.push_injected(&injected_tys)
    }

    /// Snapshot the operands surviving a suspend (everything left on the stack
    /// after the payload was popped) into the frame's `ostack`, deepest-first, and
    /// rewrite each surviving operand to read back its `i64`-erased slot. Returns
    /// the assignment statement to emit before the suspend returns (empty when
    /// nothing survives). Reference-typed survivors are unsupported (the erasure
    /// only covers the numeric types), matching the suspend payload's own limits.
    fn save_surviving_operands(&mut self) -> Result<String, TranspileError> {
        if self.stack.is_empty() {
            return Ok(String::new());
        }
        let mut encoded = Vec::with_capacity(self.stack.len());
        for val in &self.stack {
            encoded.push(encode_to_i64(&val.code, val.ty)?);
        }
        // Rewrite each survivor to read its slot; `__frame` is in scope in every
        // arm, so this stays valid across the suspend/resume boundary.
        for (i, val) in self.stack.iter_mut().enumerate() {
            val.code = decode_from_i64(&format!("__frame.ostack[{i}]"), val.ty)?;
            val.stable = true;
        }
        Ok(format!("__frame.ostack = vec![{}]; ", encoded.join(", ")))
    }

    /// Record a cross-call checkpoint as a [`Node::Checkpoint`] in the body tree
    /// (flat lowering). Like [`Self::begin_checkpoint`] it requires a clean
    /// boundary — the checkpoint sits at region depth 0, the callee takes no
    /// parameters, and the operand stack is empty (the checkpoint state clobbers
    /// `__frame.ostack` with the callee's return, so no survivor may sit there) —
    /// and allows only one per body, since the frame holds a single nested `sub`.
    /// The callee's results are pushed as operands reading back from
    /// `__frame.ostack`, where the checkpoint state stores the callee's return.
    ///
    /// Unlike `begin_checkpoint`, pending nodes in `self.cur` are fine: in the
    /// flat lowering `self.cur` is the whole body tree (earlier nodes become
    /// earlier `pc` states), not a single arm's pending statements.
    fn emit_checkpoint_node(
        &mut self,
        callee: u32,
        checkpoint: &mut Option<u32>,
    ) -> Result<(), TranspileError> {
        let results = self.check_checkpoint_boundary(callee, checkpoint, false)?;
        let save = self.save_mutated_locals()?;
        self.cur.push(Node::Checkpoint { callee, save });
        for (i, ty) in results.iter().enumerate() {
            let code = decode_from_i64(&format!("__frame.ostack[{i}]"), *ty)?;
            self.push(Val {
                code,
                ty: *ty,
                stable: true,
            });
        }
        *checkpoint = Some(callee);
        Ok(())
    }

    /// Shared validation for both cross-call checkpoint lowerings: at most one
    /// checkpoint per body, region depth 0, a no-parameter callee, and an empty
    /// operand stack. `require_empty_cur` additionally rejects pending nodes in
    /// `self.cur` — the arm-splitting path needs it (a later arm cannot see an
    /// earlier arm's pending statements), while the flat path does not (`self.cur`
    /// is the whole body tree). Returns the callee's result types.
    fn check_checkpoint_boundary(
        &self,
        callee: u32,
        checkpoint: &Option<u32>,
        require_empty_cur: bool,
    ) -> Result<Vec<ValType>, TranspileError> {
        if checkpoint.is_some() {
            return Err(TranspileError::Unsupported(
                "more than one cross-call checkpoint in a continuation body (phase 5)".into(),
            ));
        }
        if !self.frames.is_empty() {
            return Err(TranspileError::Unsupported(
                "cross-call checkpoint inside nested control flow in a continuation body (phase \
                 5b-2b)"
                    .into(),
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
        if !self.stack.is_empty() || (require_empty_cur && !self.cur.is_empty()) {
            return Err(TranspileError::Unsupported(
                "non-empty operand stack before a cross-call checkpoint (phase 5)".into(),
            ));
        }
        Ok(results.to_vec())
    }

    /// Pop and `i64`-encode a `suspend $tag`'s payload (the operand-stack tail
    /// matching the tag's parameter types) into the comma-joined form a
    /// `StepResult::Suspend` carries. Shared by both cont lowerings.
    fn encode_suspend_payload(&mut self, tag_index: u32) -> Result<String, TranspileError> {
        let payload_tys = self.suspend_payload_tys(tag_index)?;
        self.encode_stack_tail(&payload_tys)
    }

    /// Push a `suspend $tag`'s result values — the ones the resumer injects,
    /// delivered as the next step's `__args` — as the resumed state's initial
    /// operands, so the code after the suspend consumes them. Shared by both cont
    /// lowerings.
    fn push_suspend_results(&mut self, tag_index: u32) -> Result<(), TranspileError> {
        let result_tys = self
            .ctx
            .tag_results
            .get(tag_index as usize)
            .ok_or_else(|| TranspileError::Unsupported("suspend: unknown tag index".into()))?
            .clone();
        self.push_injected(&result_tys)
    }

    /// Push, as the resumed state's initial operands, the values the next step's
    /// `__args` will carry — the resumer's injection at a suspend/switch point.
    /// Each is decoded from its `i64` slot; a continuation/funcref handle rides as
    /// a `u32` (see [`Self::unerase_from_i64`]).
    fn push_injected(&mut self, tys: &[ValType]) -> Result<(), TranspileError> {
        for (i, ty) in tys.iter().enumerate() {
            let code = self.unerase_from_i64(&format!("__args[{i}]"), *ty)?;
            self.push(Val {
                code,
                ty: *ty,
                stable: true,
            });
        }
        Ok(())
    }

    /// Decode an `i64` slot back to a wasm value, extending [`decode_from_i64`]
    /// (numeric only) with the `u32`-lowering references — a `funcref`/`contref`
    /// handle — which a `switch` transfers and delivers back as a self-reference.
    /// Managed (`GcRef`) references still have no `i64` erasure and are rejected.
    fn unerase_from_i64(&self, expr: &str, ty: ValType) -> Result<String, TranspileError> {
        if matches!(ty, ValType::Ref(_)) && rust_type(ty, self.ctx.type_kinds)? == "u32" {
            return Ok(format!("(({expr}) as u32)"));
        }
        decode_from_i64(expr, ty)
    }

    /// The types a `switch $ct2 $tag` transfers to its target and receives back.
    /// `$ct2 = cont $ft2` with `$ft2 : [t1* (ref $ct_self)] -> [tr*]`: the payload
    /// `t1*` (all of `$ft2`'s parameters but the trailing self-reference) is handed
    /// to the target, and the self-reference names the continuation type
    /// `$ct_self`, whose parameters `t2*` are delivered as operands when control
    /// switches back. Returns `(t1*, t2*)`.
    fn switch_transfer_types(
        &self,
        cont_type_index: u32,
    ) -> Result<(Vec<ValType>, Vec<ValType>), TranspileError> {
        let sig = cont_underlying_sig(self.ctx.type_kinds, self.ctx.types, cont_type_index)?;
        let (self_ref, payload) = sig.params.split_last().ok_or_else(|| {
            TranspileError::Unsupported(
                "switch target continuation has no self-reference parameter".into(),
            )
        })?;
        let self_cont = concrete_ref_index(*self_ref)?;
        let injected = cont_param_types(self.ctx.type_kinds, self.ctx.types, self_cont)?;
        Ok((payload.to_vec(), injected))
    }

    /// Write the step function's opening: the `#[allow]` line, the `pub fn
    /// cont_step_func{index}` signature, the first-step parameter decode (at
    /// `pc == 0`), and the entry reload of every local from `__frame`. Both cont
    /// lowerings share this prologue; they differ only in the dispatch body that
    /// follows.
    fn write_cont_step_header(
        &self,
        index: usize,
        params: &[ValType],
        line_prefix: &str,
        src: &mut String,
    ) -> Result<(), TranspileError> {
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
            "pub fn cont_step_func{index}(&mut self, __frame: &mut ContFrame{index}, \
             __args: &[i64]) -> StepResult {{\n"
        ));
        // On the first step (`pc == 0`) the resume's injected `__args` are the
        // body's parameters (locals `0..params.len()`); decode them into their
        // frame slots before the reload below picks them up. A parameter-less
        // body has nothing to inject, so no prologue is emitted (and `__args`
        // stays unused, covered by the ALLOW attribute).
        if !params.is_empty() {
            src.push_str(line_prefix);
            src.push_str("    if __frame.pc == 0u32 {\n");
            for (i, ty) in params.iter().enumerate() {
                let decoded = self.unerase_from_i64(&format!("__args[{i}]"), *ty)?;
                src.push_str(line_prefix);
                src.push_str(&format!("        __frame.l{i} = {decoded};\n"));
            }
            src.push_str(line_prefix);
            src.push_str("    }\n");
        }
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
        Ok(())
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
        let results = self.check_checkpoint_boundary(callee, checkpoint, true)?;
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
    /// and return them comma-joined as `i64`-encoded expressions. Operands below
    /// the tail (if any) are left on the stack for the caller to handle.
    fn pop_encode_tail(&mut self, tys: &[ValType]) -> Result<String, TranspileError> {
        let mut vals = Vec::with_capacity(tys.len());
        for _ in tys {
            vals.push(self.pop()?);
        }
        vals.reverse();
        let mut encoded = Vec::with_capacity(tys.len());
        for (val, ty) in vals.iter().zip(tys) {
            encoded.push(encode_to_i64(&val.code, *ty)?);
        }
        Ok(encoded.join(", "))
    }

    /// Like [`Self::pop_encode_tail`], but require the operand stack to be exactly
    /// the tail — nothing may survive the boundary. Used where cross-state
    /// operand survival is not modelled: a function/continuation `return` (its
    /// results are the whole stack) and a top-level suspend in the arm-splitting
    /// lowering (a later arm cannot see an earlier arm's temporaries). The flat
    /// lowering instead saves survivors into the frame's `ostack` (see
    /// [`Self::save_surviving_operands`]).
    fn encode_stack_tail(&mut self, tys: &[ValType]) -> Result<String, TranspileError> {
        if self.stack.len() != tys.len() {
            return Err(TranspileError::Unsupported(
                "non-empty operand stack at a continuation suspend/return (phase 5b)".into(),
            ));
        }
        self.pop_encode_tail(tys)
    }

    /// The parameter types a `suspend $tag` consumes (the tag's parameters).
    fn suspend_payload_tys(&self, tag_index: u32) -> Result<Vec<ValType>, TranspileError> {
        Ok(self
            .ctx
            .tags
            .get(tag_index as usize)
            .ok_or_else(|| TranspileError::Unsupported("suspend: unknown tag index".into()))?
            .clone())
    }

    /// Drain the nodes queued for the current `pc` state (from `local.set`,
    /// operand spills, and any nested `block`/`loop`/`if` regions that contain no
    /// suspend) and render them into Rust source for the arm. Regions render via
    /// the ordinary [`render_nodes_into`], so their branches use Rust's own
    /// `'lN` labels and control flow — self-contained within this one state.
    fn take_arm_statements(&mut self) -> Result<String, TranspileError> {
        let nodes = std::mem::take(&mut self.cur);
        let mut out = String::new();
        render_nodes_into(nodes, 0, "", &mut out);
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
