use std::mem;

use wasmparser::{BlockType, ValType};

use super::super::{
    BrArm, Frame, FrameKind, Node, RegionNode, Val, default_value, index_u32, reachable_after,
    rust_type,
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
        // Resolve the target frame's kind and value-carrying variables (the same
        // resolution `br_table` uses per arm).
        let (is_loop, label, vars) = self.branch_arm(depth)?;

        match cond {
            None => {
                self.assign_results(&vars)?;
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
                let assigns = self.carried_assigns(&vars)?;
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
            let (is_loop, label, vars) = self.branch_arm(depth)?;
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

    /// Resolve a branch target depth to `(is_loop, label, vars)` and mark the
    /// target frame as branched to (shared by `br`/`br_if`/`br_table`). `vars`
    /// are the target's value-carrying variables (a block/if's results, or a
    /// loop's parameters), which the caller assigns before the branch. Branching
    /// to a loop targets its parameters (its loop-carried variables); branching
    /// to a block or if carries its results.
    pub(super) fn branch_arm(
        &mut self,
        depth: u32,
    ) -> Result<(bool, usize, Vec<String>), TranspileError> {
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
        Ok((is_loop, label, vars))
    }
}
