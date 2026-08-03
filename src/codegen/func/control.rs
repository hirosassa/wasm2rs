use std::mem;

use wasmparser::{BlockType, ValType};

use super::super::{
    Frame, FrameKind, Val, default_value, indent, index_u32, reachable_after, rust_type,
};
use crate::TranspileError;

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
        });
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
        let Some(frame) = self.frames.pop() else {
            return self.end_function();
        };

        // Fall-through result assignment (for blocks/loops and the else arm).
        if self.reachable {
            self.assign_results(&frame.result_vars())?;
        }

        let body = mem::take(&mut self.cur);
        let reachable_at_end = self.reachable;
        let rendered = self.render_frame(&frame, body, reachable_at_end)?;
        let next_reachable = reachable_after(&frame, reachable_at_end);

        self.cur = frame.parent_buffer;
        self.cur.extend(rendered);

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

    /// Render a finished frame into the enclosing buffer's lines.
    pub(super) fn render_frame(
        &self,
        frame: &Frame,
        body: Vec<String>,
        reachable_at_end: bool,
    ) -> Result<Vec<String>, TranspileError> {
        match frame.kind {
            FrameKind::Block | FrameKind::Loop => {
                if !frame.targeted {
                    // Never branched to: a plain sequence (a loop that is never
                    // continued to runs exactly once).
                    return Ok(body);
                }
                let mut out = vec![format!("'l{}: loop {{", frame.label)];
                out.extend(indent(&body));
                if reachable_at_end {
                    out.push(format!("    break 'l{};", frame.label));
                }
                out.push("}".to_string());
                Ok(out)
            }
            FrameKind::If => {
                let cond = frame
                    .cond
                    .clone()
                    .ok_or_else(|| TranspileError::Unsupported("if without condition".into()))?;
                let (then_lines, else_lines) = match &frame.then_buffer {
                    Some(then_lines) => (then_lines.clone(), Some(body)),
                    // No explicit `else`, but the region yields results: the
                    // implicit else forwards the parameters as the results
                    // (validation guarantees matching arity and types).
                    None if !frame.results.is_empty() => {
                        let forward = frame
                            .results
                            .iter()
                            .zip(&frame.entry_params)
                            .map(|((var, _), param)| format!("{var} = {};", param.code))
                            .collect();
                        (body, Some(forward))
                    }
                    None => (body, None),
                };

                let mut inner = vec![format!("if {cond} {{")];
                inner.extend(indent(&then_lines));
                if let Some(else_lines) = else_lines {
                    inner.push("} else {".to_string());
                    inner.extend(indent(&else_lines));
                }
                inner.push("}".to_string());

                if !frame.targeted {
                    return Ok(inner);
                }
                let mut out = vec![format!("'l{}: loop {{", frame.label)];
                out.extend(indent(&inner));
                if reachable_at_end {
                    out.push(format!("    break 'l{};", frame.label));
                }
                out.push("}".to_string());
                Ok(out)
            }
        }
    }

    pub(super) fn branch(&mut self, depth: u32, cond: Option<Val>) -> Result<(), TranspileError> {
        // Resolve the target frame's branch keyword and value-carrying variables
        // (the same resolution `br_table` uses per arm).
        let (keyword, label, vars) = self.branch_arm(depth)?;

        match cond {
            None => {
                self.assign_results(&vars)?;
                self.line(format!("{keyword} 'l{label};"));
                self.reachable = false;
                self.dead_nesting = 0;
            }
            Some(cond) if vars.is_empty() => {
                self.line(format!("if {} != 0 {{ {keyword} 'l{label}; }}", cond.code));
            }
            Some(cond) => {
                // The result values stay on the stack for the fall-through
                // path, so materialise them and reference the temporaries.
                self.spill_nonstable()?;
                let base = self
                    .stack
                    .len()
                    .checked_sub(vars.len())
                    .ok_or(TranspileError::StackUnderflow)?;
                let values: Vec<String> =
                    self.stack[base..].iter().map(|v| v.code.clone()).collect();
                self.line(format!("if {} != 0 {{", cond.code));
                for (var, value) in vars.iter().zip(&values) {
                    self.line(format!("    {var} = {value};"));
                }
                self.line(format!("    {keyword} 'l{label};"));
                self.line("}".to_string());
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
        let mut arms: Vec<(Option<u32>, u32)> = Vec::new();
        for (i, target) in targets.targets().enumerate() {
            arms.push((Some(index_u32(i)?), target?));
        }
        arms.push((None, default));

        // Every `br_table` target has the same arity, so the carried operands
        // are the same top-of-stack values for every arm; each arm just assigns
        // them to its own target's variables. After spilling they are stable, so
        // they can be referenced repeatedly across the arms.
        // The `br_table` selector is interpreted as an unsigned index.
        self.line(format!("match ({}) as u32 {{", selector.code));
        for (case, depth) in arms {
            let (keyword, label, vars) = self.branch_arm(depth)?;
            let pattern = match case {
                Some(n) => format!("{n}u32"),
                None => "_".to_string(),
            };
            if vars.is_empty() {
                self.line(format!("    {pattern} => {keyword} 'l{label},"));
            } else {
                let base = self
                    .stack
                    .len()
                    .checked_sub(vars.len())
                    .ok_or(TranspileError::StackUnderflow)?;
                let assigns: String = vars
                    .iter()
                    .zip(&self.stack[base..])
                    .map(|(var, value)| format!("{var} = {}; ", value.code))
                    .collect();
                self.line(format!(
                    "    {pattern} => {{ {assigns}{keyword} 'l{label}; }},"
                ));
            }
        }
        self.line("}".to_string());
        self.reachable = false;
        self.dead_nesting = 0;
        Ok(())
    }

    /// Resolve a branch target depth to a `(keyword, label, vars)` triple and
    /// mark the target frame as branched to (shared by `br`/`br_if`/`br_table`).
    /// `vars` are the target's value-carrying variables (a block/if's results,
    /// or a loop's parameters), which the caller assigns before the
    /// `break`/`continue`. Branching to a loop targets its parameters (its
    /// loop-carried variables); branching to a block or if carries its results.
    pub(super) fn branch_arm(
        &mut self,
        depth: u32,
    ) -> Result<(&'static str, usize, Vec<String>), TranspileError> {
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
        let keyword = if is_loop { "continue" } else { "break" };
        let label = frame.label;
        self.frames[idx].targeted = true;
        Ok((keyword, label, vars))
    }
}
