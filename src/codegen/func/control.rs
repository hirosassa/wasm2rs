use std::mem;

use wasmparser::{BlockType, ValType};

use super::super::{
    BrArm, BranchEscape, CatchArm, CatchKind, EXC_TYPE, Frame, FrameKind, Node, RegionNode, Rt,
    TryRegionNode, TryState, Val, condition_code, decode_exc_value, default_value,
    encode_exc_value, index_u32, reachable_after, rust_type,
};
use super::{CatchPhase, TryBarrier};
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

    /// `unreachable`: always traps, and code after it is dead, so emit the trap
    /// and stop lowering until the enclosing region ends (as for `return`/`br`).
    pub(super) fn emit_unreachable(&mut self) {
        self.used_rt.insert(Rt::TrapUnreachable);
        self.term("trap_unreachable();".to_string());
        self.reachable = false;
        self.dead_nesting = 0;
    }

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
                rust_type(ty, self.ctx.type_kinds)?,
                default_value(ty, self.ctx.type_kinds)
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
        self.push_frame(FrameKind::If, blockty, Some(condition_code(&cond.code)))
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
            self.line(format!(
                "let mut {name}: {} = {entry};",
                rust_type(ty, self.ctx.type_kinds)?
            ));
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
            self.line(format!("{var} = {};", self.move_val(&value)?));
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
                    let mut forward = Vec::with_capacity(results.len());
                    for ((var, _), param) in results.iter().zip(&entry_params) {
                        forward.push(Node::Line(format!("{var} = {};", self.move_val(param)?)));
                    }
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
                    cond: condition_code(&cond.code),
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
                    cond: condition_code(&cond.code),
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
                self.line(format!("if {} {{", condition_code(&cond.code)));
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

    /// Emit `target = <decoded>;` for each target, decoding the i-th `i64` slot
    /// of the runtime array named `src_array` back to its wasm type. `tys`
    /// governs the count (the Return arm decodes every result; a Suspend arm
    /// decodes only the payload prefix of the handler's result vars, leaving the
    /// trailing continuation var to the caller).
    fn assign_decoded(
        &mut self,
        targets: &[String],
        tys: &[ValType],
        src_array: &str,
    ) -> Result<(), TranspileError> {
        for (i, (target, ty)) in targets.iter().zip(tys).enumerate() {
            let decoded = super::cont::decode_from_i64(&format!("{src_array}[{i}]"), *ty)?;
            self.line(format!("{target} = {decoded};"));
        }
        Ok(())
    }

    /// `resume $ct (on $tag $lbl)...`: step the continuation handle on top of the
    /// stack once. On a normal return the decoded results are left on the stack
    /// (control falls through). On a suspension whose tag matches an on-clause,
    /// the tag payload plus the (reused, one-shot) continuation handle are
    /// carried into that clause's block and control branches there. An
    /// unmatched tag panics — propagating an unhandled suspension to a resuming
    /// caller is a later phase.
    ///
    /// The `match` is emitted as raw statements: its suspend arm branches out of
    /// an enclosing block, which the generic `Node` model has no single node
    /// for, so `resolve_target` is used only to mark the target reachable and
    /// name its label/result variables.
    pub(super) fn resume(
        &mut self,
        cont_type_index: u32,
        resume_table: &wasmparser::ResumeTable,
    ) -> Result<(), TranspileError> {
        // A `(on $tag switch)` handler turns this resume into a trampoline that
        // follows switches through a chain of continuations (see below). Its
        // on-label arms would need to hand a suspending continuation back to the
        // label using the *currently driven* handle, not the originally resumed
        // one — re-dispatching a suspend across the switch chain — which is not
        // lowered yet. Reject the mix rather than mistranslate it.
        let has_switch = resume_table
            .handlers
            .iter()
            .any(|h| matches!(h, wasmparser::Handle::OnSwitch { .. }));
        let has_label = resume_table
            .handlers
            .iter()
            .any(|h| matches!(h, wasmparser::Handle::OnLabel { .. }));
        if has_switch && has_label {
            return Err(TranspileError::Unsupported(
                "resume combining a switch handler with a suspend-to-label handler".into(),
            ));
        }

        // The handle is referenced twice (the step call and, on suspension, the
        // reused continuation), so materialise it into a stable temporary. Below
        // it on the stack sit the continuation's parameters, which this resume
        // injects: the body's parameters at its first step (empty for a
        // parameter-less continuation). Pop them (deepest-first order restored)
        // and materialise them into an `i64`-encoded slice before freezing the
        // rest of the stack so it survives the `match` unchanged.
        let handle = self.pop()?;
        let handle_var = self.fresh_temp();
        self.line(format!("let {handle_var}: u32 = {};", handle.code));

        let param_tys =
            super::cont::cont_param_types(self.ctx.type_kinds, self.ctx.types, cont_type_index)?;
        let mut args = Vec::with_capacity(param_tys.len());
        for _ in &param_tys {
            args.push(self.pop()?);
        }
        args.reverse();
        let mut encoded = Vec::with_capacity(param_tys.len());
        for (arg, ty) in args.iter().zip(&param_tys) {
            encoded.push(super::cont::encode_to_i64(&arg.code, *ty)?);
        }
        // A `(on $tag switch)` handler makes this resume a trampoline: it follows
        // each switch to a new continuation with new injected args, so the stepped
        // handle and the args live in mutable bindings a `'drive` loop rewrites.
        // Without one, the older single-step form (a fixed handle, an array of
        // args) suffices.
        let args_var = self.fresh_temp();
        if has_switch {
            self.line(format!(
                "let mut {args_var}: Vec<i64> = vec![{}];",
                encoded.join(", ")
            ));
        } else {
            self.line(format!(
                "let {args_var}: [i64; {}] = [{}];",
                param_tys.len(),
                encoded.join(", ")
            ));
        }
        self.spill_nonstable()?;

        // The Return path pushes the continuation's result values; hold them in
        // per-result temporaries assigned only on that arm (the suspend/switch
        // arms diverge via their branch or `continue`, so definite assignment
        // holds).
        let results =
            super::cont::cont_result_types(self.ctx.type_kinds, self.ctx.types, cont_type_index)?;
        let mut result_holders = Vec::with_capacity(results.len());
        for ty in &results {
            let name = self.fresh_temp();
            self.line(format!(
                "let {name}: {};",
                rust_type(*ty, self.ctx.type_kinds)?
            ));
            result_holders.push(name);
        }

        // The trampoline steps `cur_var` (initially the resumed handle); a plain
        // resume steps the fixed handle directly.
        let cur_var = self.fresh_temp();
        if has_switch {
            self.line(format!("let mut {cur_var}: u32 = {handle_var};"));
            self.line("'drive: loop {");
            self.line(format!("match self.cont_step({cur_var}, &{args_var}) {{"));
        } else {
            self.line(format!(
                "match self.cont_step({handle_var}, &{args_var}) {{"
            ));
        }
        let return_pat = if results.is_empty() { "_" } else { "__vals" };
        self.line(format!("StepResult::Return({return_pat}) => {{"));
        self.assign_decoded(&result_holders, &results, "__vals")?;
        if has_switch {
            // A returning continuation ends the trampoline; its results fall
            // through to the resume's continuation below.
            self.line("break 'drive;");
        }
        self.line("}");

        for handler in &resume_table.handlers {
            match *handler {
                wasmparser::Handle::OnLabel { tag, label } => {
                    self.emit_resume_on_label(tag, label, &handle_var)?;
                }
                wasmparser::Handle::OnSwitch { tag } => {
                    // Follow the switch: `cont_step` has already reified the parked
                    // switcher (`cur_var`), so append its handle as the target's
                    // trailing self-reference and drive the target with the switch
                    // payload as its injected args.
                    self.line(format!(
                        "StepResult::Switch {{ tag: __t, target: __tgt, args: __sa }} if __t == \
                         {tag}u32 => {{"
                    ));
                    self.line(format!(
                        "let mut __next = __sa; __next.push({cur_var} as i64);"
                    ));
                    self.line(format!("{cur_var} = __tgt; {args_var} = __next;"));
                    self.line("continue 'drive;");
                    self.line("}");
                }
            }
        }
        self.line("_ => panic!(\"resume: unhandled continuation suspend tag\"),");
        self.line("}");
        if has_switch {
            self.line("}");
        }

        for (name, ty) in result_holders.into_iter().zip(&results) {
            self.push(Val {
                code: name,
                ty: *ty,
                stable: true,
            });
        }
        Ok(())
    }

    /// Emit one `(on $tag $label)` resume handler arm: on a matching suspend,
    /// decode the tag payload into the target block's leading result variables,
    /// hand it the reused (one-shot) continuation handle in the trailing variable,
    /// and branch there. Shared by the plain and switch-driver forms of `resume`.
    fn emit_resume_on_label(
        &mut self,
        tag: u32,
        label_depth: u32,
        handle_var: &str,
    ) -> Result<(), TranspileError> {
        let payload_tys = self
            .ctx
            .tags
            .params
            .get(tag as usize)
            .ok_or_else(|| TranspileError::Unsupported("resume: unknown tag index".into()))?
            .clone();
        let (is_loop, label, vars) = match self.resolve_target(label_depth)? {
            BranchTarget::Direct {
                is_loop,
                label,
                vars,
            } => (is_loop, label, vars),
            BranchTarget::Escape { .. } => {
                return Err(TranspileError::Unsupported(
                    "resume handler crossing a try body".into(),
                ));
            }
        };
        // The handler block receives the tag payload followed by the continuation
        // reference, so it must yield exactly that arity.
        if vars.len() != payload_tys.len() + 1 {
            return Err(TranspileError::Unsupported(
                "resume handler block does not yield (payload.., contref)".into(),
            ));
        }
        let payload_pat = if payload_tys.is_empty() { "_" } else { "__pl" };
        self.line(format!(
            "StepResult::Suspend {{ tag: __t, payload: {payload_pat} }} if __t == {tag}u32 => {{"
        ));
        self.assign_decoded(&vars, &payload_tys, "__pl")?;
        let cont_var = vars.last().ok_or(TranspileError::StackUnderflow)?;
        self.line(format!("{cont_var} = {handle_var};"));
        let keyword = if is_loop { "continue" } else { "break" };
        self.line(format!("{keyword} 'l{label};"));
        self.line("}");
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
        let mut out = Vec::with_capacity(vars.len());
        for (var, value) in vars.iter().zip(&self.stack[base..]) {
            out.push((var.clone(), self.move_val(value)?));
        }
        Ok(out)
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
        if let Some(&TryBarrier {
            frame_idx: try_idx,
            phase: CatchPhase::Handler,
        }) = self.try_barriers.last()
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
            .find(|b| b.phase == CatchPhase::Body)
            .map(|b| b.frame_idx)
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
        self.try_barriers.push(TryBarrier {
            frame_idx: idx,
            phase: CatchPhase::Body,
        });
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
                let params = self
                    .ctx
                    .tags
                    .params
                    .get(t as usize)
                    .cloned()
                    .ok_or_else(|| TranspileError::Unsupported("catch of an unknown tag".into()))?;
                let mut binds = Vec::with_capacity(params.len());
                let mut pushed = Vec::with_capacity(params.len());
                for (i, ty) in params.iter().enumerate() {
                    let var = format!("__hv{label}_{i}");
                    let source =
                        format!("{exc_var}.downcast_ref::<{EXC_TYPE}>().unwrap().values[{i}]");
                    binds.push(format!(
                        "let {var}: {} = {};",
                        rust_type(*ty, self.ctx.type_kinds)?,
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
            barrier.phase = CatchPhase::Handler;
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
        self.emit_throw_payload(tag_index)
    }

    /// Pop tag `tag_index`'s payload off the operand stack (top-first) and raise
    /// it as a `panic_any` of [`EXC_TYPE`], marking the program point unreachable.
    /// Shared by `throw` and `resume_throw` (which first pops and consumes the
    /// continuation it abandons).
    fn emit_throw_payload(&mut self, tag_index: u32) -> Result<(), TranspileError> {
        self.uses_eh = true;
        let params = self
            .ctx
            .tags
            .params
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

    /// `resume_throw $ct $exn`: resume a suspended continuation by raising
    /// exception `$exn` at its suspension point instead of continuing normally.
    /// A continuation body cannot (yet) install an exception handler, so the
    /// injected exception always propagates straight out to here — equivalent to
    /// abandoning the continuation and raising the exception in the resumer. The
    /// resume handlers would only fire on a re-suspend after an *internal* catch,
    /// which cannot happen, so a non-empty table is rejected.
    pub(super) fn resume_throw(
        &mut self,
        cont_type_index: u32,
        tag_index: u32,
        resume_table: &wasmparser::ResumeTable,
    ) -> Result<(), TranspileError> {
        super::cont::require_cont_type(self.ctx.type_kinds, cont_type_index)?;
        if !resume_table.handlers.is_empty() {
            return Err(TranspileError::Unsupported(
                "resume_throw with suspend handlers (needs an exception handler inside the \
                 continuation body)"
                    .into(),
            ));
        }
        // The continuation reference sits on top of the exception payload; pop and
        // consume it (one-shot — the throw abandons it) before raising the payload.
        let handle = self.pop()?;
        let handle_var = self.fresh_temp();
        self.line(format!("let {handle_var}: u32 = {};", handle.code));
        self.line(format!("let _ = self.conts[{handle_var} as usize].take();"));
        self.emit_throw_payload(tag_index)
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
