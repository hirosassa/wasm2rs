use std::mem;

use wasmparser::{BlockType, ValType};

use super::super::{
    BrArm, BranchEscape, CatchArm, CatchKind, EXC_TYPE, Frame, FrameKind, Node, RegionNode,
    TryRegionNode, TryState, Val, decode_exc_value, default_value, encode_exc_value, index_u32,
    reachable_after, rust_type,
};
use crate::TranspileError;

/// A resolved branch target: either a plain `break`/`continue` to an enclosing
/// region, or one that leaves an enclosing `try` body and must be re-issued from
/// outside the `catch_unwind` closure.
enum BranchTarget {
    Direct {
        is_loop: bool,
        label: usize,
        vars: Vec<String>,
    },
    Escape {
        target_idx: usize,
        is_loop: bool,
        label: usize,
        vars: Vec<String>,
    },
}

impl<'a> super::FuncGen<'a> {
    // ----- control flow ----------------------------------------------------

    /// Resolve a block type to its `(parameter types, result types)`.
    pub(super) fn block_signature(
        &self,
        blockty: BlockType,
    ) -> Result<(Vec<ValType>, Vec<ValType>), TranspileError> {
        match blockty {
            BlockType::Empty => Ok((Vec::new(), Vec::new())),
            BlockType::Type(ty) => Ok((Vec::new(), vec![ty])),
            BlockType::FuncType(idx) => {
                let sig = self
                    .ctx
                    .types
                    .get(idx as usize)
                    .ok_or_else(|| TranspileError::Unsupported("block: unknown type".into()))?;
                Ok((sig.params.clone(), sig.results.clone()))
            }
        }
    }

    /// Allocate one default-initialised `let mut` variable per result type so
    /// every control path is definitely assigned before the region's value is
    /// read.
    pub(super) fn alloc_results(
        &mut self,
        result_types: &[ValType],
    ) -> Result<Vec<(String, ValType)>, TranspileError> {
        let mut results = Vec::with_capacity(result_types.len());
        for &ty in result_types {
            let name = self.fresh_temp();
            self.line(format!(
                "let mut {name}: {} = {};",
                rust_type(ty)?,
                default_value(ty)
            ));
            results.push((name, ty));
        }
        Ok(results)
    }

    pub(super) fn open_frame(
        &mut self,
        kind: FrameKind,
        blockty: BlockType,
    ) -> Result<(), TranspileError> {
        self.push_frame(kind, blockty, None)
    }

    pub(super) fn open_if(&mut self, blockty: BlockType) -> Result<(), TranspileError> {
        // The condition is consumed before the surrounding stack is spilled, so
        // it is popped here rather than inside `push_frame`.
        let cond = self.pop()?;
        self.push_frame(FrameKind::If, blockty, Some(format!("{} != 0", cond.code)))
    }

    /// Spill the operand stack, allocate a label, and push a fresh frame that
    /// captures the enclosing scope's height and output buffer.
    pub(super) fn push_frame(
        &mut self,
        kind: FrameKind,
        blockty: BlockType,
        cond: Option<String>,
    ) -> Result<(), TranspileError> {
        let (param_types, result_types) = self.block_signature(blockty)?;
        self.spill_nonstable()?;
        // The parameters are the top operands; they stay on the stack as the
        // region's initial values, so the enclosing scope ends below them.
        let parent_height = self
            .stack
            .len()
            .checked_sub(param_types.len())
            .ok_or(TranspileError::StackUnderflow)?;
        let entry_params = self.stack[parent_height..].to_vec();
        // A loop's parameters are loop-carried, so they become mutable variables
        // that a `br` back to the header can reassign. A block's/`if`'s
        // parameters are read-only and stay as their entry expressions.
        let loop_params = if kind == FrameKind::Loop {
            self.materialize_loop_params(parent_height, &param_types)?
        } else {
            Vec::new()
        };
        // Result variables are declared in the enclosing buffer, before the
        // region, so a `br` out of it (or its fall-through) can assign them.
        let results = self.alloc_results(&result_types)?;
        let label = self.label_counter;
        self.label_counter += 1;
        let parent_buffer = mem::take(&mut self.cur);
        self.frames.push(Frame {
            kind,
            label,
            targeted: false,
            results,
            entry_params,
            loop_params,
            parent_height,
            parent_buffer,
            then_buffer: None,
            then_reachable: false,
            cond,
            try_state: None,
        });
        self.max_depth = self.max_depth.max(self.frames.len());
        Ok(())
    }

    /// Turn a loop's entry parameters into `let mut` variables (initialised to
    /// their entry expressions) and rewrite their stack slots to reference the
    /// variables, so the loop body and any `br` back to it share the same
    /// loop-carried storage.
    pub(super) fn materialize_loop_params(
        &mut self,
        parent_height: usize,
        param_types: &[ValType],
    ) -> Result<Vec<(String, ValType)>, TranspileError> {
        let mut vars = Vec::with_capacity(param_types.len());
        for (i, &ty) in param_types.iter().enumerate() {
            let name = self.fresh_temp();
            let entry = self.stack[parent_height + i].code.clone();
            self.line(format!("let mut {name}: {} = {entry};", rust_type(ty)?));
            self.stack[parent_height + i] = Val {
                code: name.clone(),
                ty,
                stable: true,
            };
            vars.push((name, ty));
        }
        Ok(vars)
    }

    /// Assign the current frame's results from the top operands (in source
    /// order, popped last-first).
    pub(super) fn assign_fallthrough_result(&mut self) -> Result<(), TranspileError> {
        let vars = self
            .frames
            .last()
            .map(Frame::result_vars)
            .unwrap_or_default();
        self.assign_results(&vars)
    }

    /// Pop one value per variable and assign them, so `vars[i]` receives the
    /// i-th source-order operand.
    pub(super) fn assign_results(&mut self, vars: &[String]) -> Result<(), TranspileError> {
        for var in vars.iter().rev() {
            let value = self.pop()?;
            self.line(format!("{var} = {};", value.code));
        }
        Ok(())
    }

    pub(super) fn handle_else(&mut self) -> Result<(), TranspileError> {
        if self.reachable {
            self.assign_fallthrough_result()?;
        }
        let then_lines = mem::take(&mut self.cur);
        let reachable = self.reachable;
        let frame = self
            .frames
            .last_mut()
            .ok_or(TranspileError::StackUnderflow)?;
        if frame.kind != FrameKind::If {
            return Err(TranspileError::Unsupported("else without if".into()));
        }
        frame.then_reachable = reachable;
        frame.then_buffer = Some(then_lines);
        let parent_height = frame.parent_height;
        let entry_params = frame.entry_params.clone();
        // The `then` arm consumed the parameters; the `else` arm starts with the
        // same (stable) parameter values on the stack.
        self.stack.truncate(parent_height);
        self.stack.extend(entry_params);
        self.reachable = true;
        Ok(())
    }

    pub(super) fn handle_end(&mut self) -> Result<(), TranspileError> {
        // A `try` region ends into a `catch_unwind` node, not the generic
        // block/loop/if lowering.
        if self.frames.last().is_some_and(|f| f.try_state.is_some()) {
            return self.handle_try_end();
        }
        let Some(frame) = self.frames.pop() else {
            return self.end_function();
        };

        // Fall-through result assignment (for blocks/loops and the else arm).
        if self.reachable {
            self.assign_results(&frame.result_vars())?;
        }

        let body = mem::take(&mut self.cur);
        let reachable_at_end = self.reachable;
        let next_reachable = reachable_after(&frame, reachable_at_end);

        let Frame {
            kind,
            label,
            targeted,
            results,
            entry_params,
            parent_height,
            parent_buffer,
            then_buffer,
            cond,
            ..
        } = frame;

        // Split an `if` into its then/else arms. When the region yields results
        // but has no explicit `else`, synthesise an implicit else that forwards
        // the parameters to the results (validation guarantees matching arity
        // and types). Blocks and loops keep their whole body as-is.
        let (body, els) = match kind {
            FrameKind::Block | FrameKind::Loop => (body, None),
            FrameKind::If => match then_buffer {
                Some(then_nodes) => (then_nodes, Some(body)),
                None if !results.is_empty() => {
                    let forward = results
                        .iter()
                        .zip(&entry_params)
                        .map(|((var, _), param)| Node::Line(format!("{var} = {};", param.code)))
                        .collect();
                    (body, Some(forward))
                }
                None => (body, None),
            },
        };

        let region = RegionNode {
            kind,
            label,
            targeted,
            reachable_at_end,
            cond,
            body,
            els,
        };

        self.cur = parent_buffer;
        self.cur.push(Node::Region(region));

        self.stack.truncate(parent_height);
        for (var, ty) in results {
            self.push(Val {
                code: var,
                ty,
                stable: true,
            });
        }

        self.reachable = next_reachable;
        if !self.reachable {
            self.dead_nesting = 0;
        }
        Ok(())
    }

    pub(super) fn branch(&mut self, depth: u32, cond: Option<Val>) -> Result<(), TranspileError> {
        match self.resolve_target(depth)? {
            BranchTarget::Direct {
                is_loop,
                label,
                vars,
            } => self.emit_direct_branch(is_loop, label, &vars, cond),
            BranchTarget::Escape {
                target_idx,
                is_loop,
                label,
                vars,
            } => self.emit_escaping_branch(target_idx, is_loop, label, &vars, cond),
        }
    }

    /// Emit a branch that stays within the current `catch_unwind` closure (if
    /// any): a plain `break`/`continue`, optionally guarded by `br_if`'s
    /// condition.
    fn emit_direct_branch(
        &mut self,
        is_loop: bool,
        label: usize,
        vars: &[String],
        cond: Option<Val>,
    ) -> Result<(), TranspileError> {
        match cond {
            None => {
                self.assign_results(vars)?;
                self.node(Node::Br { label, is_loop });
                self.reachable = false;
                self.dead_nesting = 0;
            }
            Some(cond) if vars.is_empty() => {
                self.node(Node::BrIf {
                    cond: cond.code,
                    label,
                    is_loop,
                    assigns: Vec::new(),
                });
            }
            Some(cond) => {
                // The result values stay on the stack for the fall-through
                // path, so materialise them and reference the temporaries.
                self.spill_nonstable()?;
                let assigns = self.carried_assigns(vars)?;
                self.node(Node::BrIf {
                    cond: cond.code,
                    label,
                    is_loop,
                    assigns,
                });
            }
        }
        Ok(())
    }

    /// Emit a branch that leaves an enclosing `try` body: carry its values into
    /// the (outer) target's variables, then set the try's outcome variable and
    /// `return` from the closure. The try's post-`match` dispatch turns the
    /// outcome back into the real branch.
    fn emit_escaping_branch(
        &mut self,
        target_idx: usize,
        is_loop: bool,
        label: usize,
        vars: &[String],
        cond: Option<Val>,
    ) -> Result<(), TranspileError> {
        match cond {
            None => {
                self.assign_results(vars)?;
                let (code, out_var) = self.record_branch_escape(target_idx, is_loop, label)?;
                self.line(format!("{out_var} = {code}u32;"));
                self.term("return;");
                self.reachable = false;
                self.dead_nesting = 0;
            }
            Some(cond) => {
                // Only the taken path escapes; the values are carried inside the
                // guard so the fall-through leaves them on the stack untouched.
                self.spill_nonstable()?;
                let assigns = self.carried_assigns(vars)?;
                let (code, out_var) = self.record_branch_escape(target_idx, is_loop, label)?;
                self.line(format!("if {} != 0 {{", cond.code));
                for (var, value) in assigns {
                    self.line(format!("{var} = {value};"));
                }
                self.line(format!("{out_var} = {code}u32;"));
                self.line("return;");
                self.line("}");
            }
        }
        Ok(())
    }

    pub(super) fn branch_table(
        &mut self,
        targets: wasmparser::BrTable<'_>,
    ) -> Result<(), TranspileError> {
        let selector = self.pop()?;
        self.spill_nonstable()?;

        let default = targets.default();
        let mut cases: Vec<(Option<u32>, u32)> = Vec::new();
        for (i, target) in targets.targets().enumerate() {
            cases.push((Some(index_u32(i)?), target?));
        }
        cases.push((None, default));

        // Every `br_table` target has the same arity, so the carried operands
        // are the same top-of-stack values for every arm; each arm just assigns
        // them to its own target's variables. After spilling they are stable, so
        // they can be referenced repeatedly across the arms.
        let mut arms = Vec::with_capacity(cases.len());
        for (case, depth) in cases {
            // A `br_table` whose arms leave a `try` body (mixing escaping and
            // non-escaping targets) is not lowered; only plain targets are.
            let (is_loop, label, vars) = match self.resolve_target(depth)? {
                BranchTarget::Direct {
                    is_loop,
                    label,
                    vars,
                } => (is_loop, label, vars),
                BranchTarget::Escape { .. } => {
                    return Err(TranspileError::Unsupported(
                        "br_table out of a try region".into(),
                    ));
                }
            };
            let pattern = match case {
                Some(n) => format!("{n}u32"),
                None => "_".to_string(),
            };
            let assigns = self.carried_assigns(&vars)?;
            arms.push(BrArm {
                pattern,
                label,
                is_loop,
                assigns,
            });
        }
        self.node(Node::BrTable {
            selector: selector.code,
            arms,
        });
        self.reachable = false;
        self.dead_nesting = 0;
        Ok(())
    }

    /// Pair each carried variable with the operand-stack value it receives (the
    /// top `vars.len()` operands, which `spill_nonstable` has already made
    /// stable so they can be referenced across arms).
    fn carried_assigns(&self, vars: &[String]) -> Result<Vec<(String, String)>, TranspileError> {
        let base = self
            .stack
            .len()
            .checked_sub(vars.len())
            .ok_or(TranspileError::StackUnderflow)?;
        Ok(vars
            .iter()
            .zip(&self.stack[base..])
            .map(|(var, value)| (var.clone(), value.code.clone()))
            .collect())
    }

    /// Resolve a branch target depth to a [`BranchTarget`] and mark the target
    /// frame as branched to (shared by `br`/`br_if`/`br_table`). The `vars` are
    /// the target's value-carrying variables (a block/if's results, or a loop's
    /// parameters), which the caller assigns before the branch. A branch that
    /// leaves an enclosing `try` *body* resolves to [`BranchTarget::Escape`]; one
    /// leaving a `try` *handler* has no lowering (the landing pad is not a
    /// breakable region) and is rejected.
    fn resolve_target(&mut self, depth: u32) -> Result<BranchTarget, TranspileError> {
        let idx = self
            .frames
            .len()
            .checked_sub(1 + depth as usize)
            .ok_or_else(|| TranspileError::Unsupported("branch depth out of range".into()))?;
        let frame = &self.frames[idx];
        let is_loop = frame.kind == FrameKind::Loop;
        let vars = if is_loop {
            frame.loop_param_vars()
        } else {
            frame.result_vars()
        };
        let label = frame.label;
        self.frames[idx].targeted = true;

        // A branch to the try whose *handler* we are directly in has no lowering:
        // that try's body loop lives inside its `catch_unwind` closure, which the
        // landing pad (where the handler runs) sits outside of. A branch to any
        // other target from a handler is an ordinary break/continue.
        if let Some(&(try_idx, true)) = self.try_barriers.last()
            && idx == try_idx
        {
            return Err(TranspileError::Unsupported(
                "branch out of a try handler".into(),
            ));
        }
        // A branch leaving the innermost enclosing try *body* crosses that
        // closure and must be re-issued from outside it via the outcome signal.
        if self.branch_escapes_try(idx) {
            return Ok(BranchTarget::Escape {
                target_idx: idx,
                is_loop,
                label,
                vars,
            });
        }
        Ok(BranchTarget::Direct {
            is_loop,
            label,
            vars,
        })
    }

    /// Record a branch that escapes the innermost enclosing `try` body, returning
    /// the `(outcome code, outcome variable)` to signal from the closure. Escapes
    /// to the same target share a code so one dispatch arm re-issues them all.
    fn record_branch_escape(
        &mut self,
        target_idx: usize,
        is_loop: bool,
        label: usize,
    ) -> Result<(u32, String), TranspileError> {
        let try_idx = self.enclosing_try_body().ok_or_else(|| {
            TranspileError::Unsupported("branch escape outside a try body".into())
        })?;
        let try_label = self.frames[try_idx].label;
        let ts = self.frames[try_idx]
            .try_state
            .as_mut()
            .ok_or_else(|| TranspileError::Unsupported("branch escape outside a try".into()))?;
        let code = match ts.escapes.iter().position(|e| e.target_idx == target_idx) {
            Some(pos) => pos + 1,
            None => {
                ts.escapes.push(BranchEscape {
                    target_idx,
                    is_loop,
                    label,
                });
                ts.escapes.len()
            }
        };
        Ok((index_u32(code)?, format!("__out{try_label}")))
    }

    /// Emit a `try`'s post-`match` dispatch: turn each recorded outcome back into
    /// the real control transfer, outside the `catch_unwind` closure. A target
    /// still inside an enclosing try body becomes another closure-outcome signal
    /// (propagating the escape outward); otherwise it is a plain `break`/
    /// `continue`. A return escape is re-issued the same way.
    fn emit_try_dispatch(
        &mut self,
        out_var: &str,
        escapes: Vec<BranchEscape>,
        has_ret_escape: bool,
    ) -> Result<(), TranspileError> {
        // At most one signal is set per closure exit, so the checks are
        // independent and their order does not matter.
        if has_ret_escape {
            self.emit_return_dispatch()?;
        }
        for (i, esc) in escapes.into_iter().enumerate() {
            let code = index_u32(i + 1)?;
            self.line(format!("if {out_var} == {code}u32 {{"));
            if self.branch_escapes_try(esc.target_idx) {
                // Still inside an enclosing try body: re-signal that closure. The
                // carried values already sit in the target's variables.
                let (code2, out2) =
                    self.record_branch_escape(esc.target_idx, esc.is_loop, esc.label)?;
                self.line(format!("{out2} = {code2}u32;"));
                self.line("return;");
            } else {
                let keyword = if esc.is_loop { "continue" } else { "break" };
                self.line(format!("{keyword} 'l{};", esc.label));
            }
            self.line("}");
        }
        Ok(())
    }

    /// Emit the return-escape arm of a `try`'s dispatch: if the dispatch itself
    /// sits in an enclosing try body, leave that closure too (marking it so it
    /// re-dispatches); otherwise perform the real function `return`.
    fn emit_return_dispatch(&mut self) -> Result<(), TranspileError> {
        let enclosing = self.enclosing_try_body();
        self.line("if __returning {");
        match enclosing {
            Some(_) => {
                self.mark_enclosing_ret_escape();
                self.line("return;");
            }
            None => {
                let expr = self.return_holder_expr();
                if expr.is_empty() {
                    self.line("return;");
                } else {
                    self.line(format!("return {expr};"));
                }
            }
        }
        self.line("}");
        Ok(())
    }

    /// The expression yielding the function's result(s) from the return holders
    /// (`__rv{i}`): empty for no results, a bare holder for one, a tuple for more.
    fn return_holder_expr(&self) -> String {
        let n = self.results.len();
        if n == 0 {
            return String::new();
        }
        let parts: Vec<String> = (0..n).map(|i| format!("__rv{i}")).collect();
        if n == 1 {
            parts.join(", ")
        } else {
            format!("({})", parts.join(", "))
        }
    }

    /// Flag the innermost enclosing `try` body so its dispatch re-issues the
    /// function return — used when a return escape leaves that closure too.
    fn mark_enclosing_ret_escape(&mut self) {
        if let Some(idx) = self.enclosing_try_body()
            && let Some(ts) = self.frames[idx].try_state.as_mut()
        {
            ts.has_ret_escape = true;
        }
    }

    /// Emit a `return` that escapes an enclosing `try` body: stash the results in
    /// the function-wide holders, raise the return signal, and leave the closure.
    /// Each enclosing try's dispatch re-issues it until the real function return.
    pub(super) fn emit_return_escape(&mut self) -> Result<(), TranspileError> {
        self.uses_ret_escape = true;
        // Pop the results highest-index first (as `assign_results` does) so holder
        // `__rv{i}` receives the i-th source-order operand.
        for i in (0..self.results.len()).rev() {
            let val = self.pop()?;
            self.line(format!("__rv{i} = {};", val.code));
        }
        self.line("__returning = true;");
        self.term("return;");
        // The innermost enclosing try body owns the closure this return leaves;
        // its dispatch must re-issue the function return.
        self.mark_enclosing_ret_escape();
        self.reachable = false;
        self.dead_nesting = 0;
        Ok(())
    }

    // ----- legacy exception handling ---------------------------------------

    /// The frame index of the innermost enclosing `try` *body* around the current
    /// point, i.e. the innermost `catch_unwind` closure a branch would sit inside.
    /// A catch handler runs in the landing pad, *outside* its own try's closure,
    /// so a barrier in the catch phase does not count as an enclosing body.
    fn enclosing_try_body(&self) -> Option<usize> {
        self.try_barriers
            .iter()
            .rev()
            .find(|&&(_, in_catch)| !in_catch)
            .map(|&(idx, _)| idx)
    }

    /// Whether a branch to `target_idx` would leave the innermost enclosing `try`
    /// body — crossing its `catch_unwind` closure, which a `break`/`continue`
    /// cannot do, so it must be re-issued via the closure-outcome signal. A
    /// target at or inside that body stays within the closure (a plain branch).
    fn branch_escapes_try(&self, target_idx: usize) -> bool {
        self.enclosing_try_body()
            .is_some_and(|body_idx| target_idx < body_idx)
    }

    /// Open a `try` region. It reuses a block frame (a `br` to it exits like a
    /// block) but carries [`TryState`] so `end` lowers it to a `catch_unwind`,
    /// and registers a barrier so branches leaving its body are re-issued from
    /// outside the closure.
    pub(super) fn open_try(
        &mut self,
        blockty: wasmparser::BlockType,
    ) -> Result<(), TranspileError> {
        self.uses_eh = true;
        self.push_frame(FrameKind::Block, blockty, None)?;
        let idx = self
            .frames
            .len()
            .checked_sub(1)
            .ok_or(TranspileError::StackUnderflow)?;
        let label = self.frames[idx].label;
        self.frames[idx].try_state = Some(TryState {
            exc_var: format!("__exc{label}"),
            arms: Vec::new(),
            cur_kind: CatchKind::Body,
            cur_binds: Vec::new(),
            escapes: Vec::new(),
            has_ret_escape: false,
        });
        // The barrier starts in the body phase; the first catch flips it so the
        // handler no longer counts as inside this try's closure (only a branch to
        // the try itself from its handler is then rejected).
        self.try_barriers.push((idx, false));
        Ok(())
    }

    /// Handle a `catch $tag` (`tag` is `Some`) or `catch_all` (`None`): close the
    /// current arm, reset the operand stack, bind the exception payload (for a
    /// tagged catch) and start the handler.
    pub(super) fn handle_catch(&mut self, tag: Option<u32>) -> Result<(), TranspileError> {
        // The falling-through arm produces the try's results before it ends.
        if self.reachable {
            self.assign_fallthrough_result()?;
        }
        let body = mem::take(&mut self.cur);
        let reachable_at_end = self.reachable;

        let idx = self
            .frames
            .len()
            .checked_sub(1)
            .ok_or(TranspileError::StackUnderflow)?;
        let frame = self.frames.get(idx).ok_or(TranspileError::StackUnderflow)?;
        let ts = frame
            .try_state
            .as_ref()
            .ok_or_else(|| TranspileError::Unsupported("catch without try".into()))?;
        let exc_var = ts.exc_var.clone();
        let parent_height = frame.parent_height;
        let label = frame.label;

        // Build the new handler's payload bindings and the operands it pushes.
        let (new_kind, binds, pushed) = match tag {
            Some(t) => {
                let params =
                    self.ctx.tags.get(t as usize).cloned().ok_or_else(|| {
                        TranspileError::Unsupported("catch of an unknown tag".into())
                    })?;
                let mut binds = Vec::with_capacity(params.len());
                let mut pushed = Vec::with_capacity(params.len());
                for (i, ty) in params.iter().enumerate() {
                    let var = format!("__hv{label}_{i}");
                    let source =
                        format!("{exc_var}.downcast_ref::<{EXC_TYPE}>().unwrap().values[{i}]");
                    binds.push(format!(
                        "let {var}: {} = {};",
                        rust_type(*ty)?,
                        decode_exc_value(*ty, &source)?
                    ));
                    pushed.push(Val {
                        code: var,
                        ty: *ty,
                        stable: true,
                    });
                }
                (CatchKind::Tag(t), binds, pushed)
            }
            None => (CatchKind::All, Vec::new(), Vec::new()),
        };

        // Close the previous arm and start the new one.
        if let Some(ts) = self.frames[idx].try_state.as_mut() {
            let kind = mem::replace(&mut ts.cur_kind, new_kind);
            let prev_binds = mem::replace(&mut ts.cur_binds, binds);
            ts.arms.push(CatchArm {
                kind,
                binds: prev_binds,
                body,
                reachable_at_end,
            });
        }
        // Handler code runs in the landing pad, outside the body's labelled
        // loop, so flip this try's barrier to the catch phase. Every nested try
        // opened in the body has already ended, so this try's barrier is on top.
        if let Some(barrier) = self.try_barriers.last_mut() {
            barrier.1 = true;
        }

        self.stack.truncate(parent_height);
        for val in pushed {
            self.push(val);
        }
        self.reachable = true;
        Ok(())
    }

    /// Finish a `try` region, assembling its body and handlers into a
    /// [`Node::Try`] emitted into the enclosing buffer.
    pub(super) fn handle_try_end(&mut self) -> Result<(), TranspileError> {
        if self.reachable {
            self.assign_fallthrough_result()?;
        }
        let body = mem::take(&mut self.cur);
        let reachable_at_end = self.reachable;

        let mut frame = self.frames.pop().ok_or(TranspileError::StackUnderflow)?;
        let ts = frame
            .try_state
            .take()
            .ok_or_else(|| TranspileError::Unsupported("try end without try".into()))?;
        let TryState {
            exc_var,
            mut arms,
            cur_kind,
            cur_binds,
            escapes,
            has_ret_escape,
        } = ts;
        arms.push(CatchArm {
            kind: cur_kind,
            binds: cur_binds,
            body,
            reachable_at_end,
        });
        // This try's barrier (pushed on open, flipped at its first catch) is on
        // top since every nested try has already ended.
        self.try_barriers.pop();

        // The try continues if any arm falls through, or a `br` targets it.
        let next_reachable = frame.targeted || arms.iter().any(|a| a.reachable_at_end);

        // The first arm is always the protected body; the rest are handlers.
        let mut arms = arms.into_iter();
        let body_arm = arms
            .next()
            .ok_or_else(|| TranspileError::Unsupported("try without a body".into()))?;
        let catches: Vec<CatchArm> = arms.collect();

        let node = Node::Try(TryRegionNode {
            label: frame.label,
            targeted: frame.targeted,
            exc_var,
            body: body_arm.body,
            body_reachable_at_end: body_arm.reachable_at_end,
            catches,
        });

        let out_var = format!("__out{}", frame.label);
        self.cur = mem::take(&mut frame.parent_buffer);
        // Branches escaping the body signal an outcome; declare its variable
        // before the `match` (so a try inside a loop re-initialises it each
        // iteration), then dispatch the recorded outcomes after it.
        let has_escapes = !escapes.is_empty();
        if has_escapes {
            self.line(format!("let mut {out_var}: u32 = 0;"));
        }
        self.node(node);
        if has_escapes || has_ret_escape {
            self.emit_try_dispatch(&out_var, escapes, has_ret_escape)?;
        }
        // The try lowers to a `()`-typed `match`. When it never falls through
        // (its body and every handler diverge), the following wasm code is dead
        // and not emitted, so a diverging guard stands in for it — otherwise the
        // `match` would be a `()` tail where a value is expected. It is never
        // reached, since every path through the `match` diverges.
        if !next_reachable {
            self.term("unreachable!();".to_string());
        }

        self.stack.truncate(frame.parent_height);
        for (var, ty) in frame.results {
            self.push(Val {
                code: var,
                ty,
                stable: true,
            });
        }
        self.reachable = next_reachable;
        if !self.reachable {
            self.dead_nesting = 0;
        }
        Ok(())
    }

    /// Emit a `throw $tag`: pop the payload operands, bit-encode them, and raise
    /// the exception as a `panic_any` so an enclosing `try` can catch it.
    pub(super) fn emit_throw(&mut self, tag_index: u32) -> Result<(), TranspileError> {
        self.uses_eh = true;
        let params = self
            .ctx
            .tags
            .get(tag_index as usize)
            .cloned()
            .ok_or_else(|| TranspileError::Unsupported("throw of an unknown tag".into()))?;
        // Operands are popped top-first, so fill the payload back to front.
        let mut encoded = vec![String::new(); params.len()];
        for i in (0..params.len()).rev() {
            let value = self.pop()?;
            encoded[i] = encode_exc_value(params[i], &value.code)?;
        }
        self.term(format!(
            "::std::panic::panic_any({EXC_TYPE} {{ tag: {tag_index}u32, values: vec![{}] }});",
            encoded.join(", ")
        ));
        self.reachable = false;
        self.dead_nesting = 0;
        Ok(())
    }

    /// Emit a `rethrow`: re-raise the exception caught by the targeted enclosing
    /// `try`'s handler.
    pub(super) fn emit_rethrow(&mut self, relative_depth: u32) -> Result<(), TranspileError> {
        let idx = self
            .frames
            .len()
            .checked_sub(1 + relative_depth as usize)
            .ok_or_else(|| TranspileError::Unsupported("rethrow depth out of range".into()))?;
        let exc_var = self
            .frames
            .get(idx)
            .and_then(|f| f.try_state.as_ref())
            .map(|ts| ts.exc_var.clone())
            .ok_or_else(|| TranspileError::Unsupported("rethrow target is not a try".into()))?;
        self.term(format!("::std::panic::resume_unwind({exc_var});"));
        self.reachable = false;
        self.dead_nesting = 0;
        Ok(())
    }
}
