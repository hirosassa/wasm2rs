//! Per-function code generation.
//!
//! wasm bytecode is already structured — `block`/`loop`/`if` regions nest
//! properly and `br N` can only target one of the `N` enclosing labels, so
//! there is never an irreducible control-flow graph. Each region therefore
//! maps directly onto Rust: a `block` or `loop` that is actually branched to
//! becomes a labelled `loop { ... }` (with `break`/`continue` for `br`), while
//! one that is never targeted is emitted inline to avoid unused-label warnings.
//!
//! Values are tracked on a simulated operand stack of expression strings. A
//! value is "stable" when re-evaluating its expression always yields the same
//! result (a constant, an immutable local, a materialised temporary, or a
//! combination of those). Only non-stable values are spilled into `let`
//! bindings at control-flow boundaries and before local mutations, which keeps
//! straight-line code compiling to clean inline expressions.

use std::collections::HashSet;
use std::mem;

use wasmparser::{BlockType, FunctionBody, Operator, ValType};

use crate::TranspileError;

/// A value on the simulated operand stack.
#[derive(Clone)]
struct Val {
    /// The Rust expression that produces this value.
    code: String,
    ty: ValType,
    /// Whether re-evaluating `code` is guaranteed to yield the same result.
    stable: bool,
}

/// The kind of a structured control-flow region.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FrameKind {
    Block,
    Loop,
    If,
}

/// One active control-flow region.
struct Frame {
    kind: FrameKind,
    /// Numeric label; only rendered as `'lN` if the frame is branched to.
    label: usize,
    /// Set when some `br`/`br_if`/`br_table` targets this frame.
    targeted: bool,
    /// Result variable and type, when the region yields a value.
    result: Option<(String, ValType)>,
    /// Operand-stack height of the enclosing scope (values below this frame).
    parent_height: usize,
    /// The output buffer of the enclosing scope, restored when the frame ends.
    parent_buffer: Vec<String>,
    /// For `if`: the `then` branch lines, captured when `else` is reached.
    then_buffer: Option<Vec<String>>,
    /// For `if`: whether the `then` branch could fall through to `else`.
    then_reachable: bool,
    /// For `if`: the Rust condition expression.
    cond: Option<String>,
}

pub(crate) fn generate_function(
    index: usize,
    params: &[ValType],
    results: &[ValType],
    body: &FunctionBody<'_>,
) -> Result<String, TranspileError> {
    let result = match results {
        [] => None,
        [ty] => Some(*ty),
        _ => return Err(TranspileError::Unsupported("multi-value results".into())),
    };

    let mut func = FuncGen::new(params, result, body)?;
    func.run(body)?;
    func.finish(index, params, result)
}

/// State threaded through the translation of a single function body.
struct FuncGen {
    local_types: Vec<ValType>,
    mutable_locals: HashSet<u32>,
    result: Option<ValType>,
    stack: Vec<Val>,
    frames: Vec<Frame>,
    /// The output buffer of the innermost scope currently being emitted into.
    cur: Vec<String>,
    temp_counter: usize,
    label_counter: usize,
    /// Whether the current program point is reachable.
    reachable: bool,
    /// Nesting depth of regions opened while unreachable (skipped wholesale).
    dead_nesting: usize,
    /// The tail expression returned by the function, if any.
    trailing: Option<String>,
}

impl FuncGen {
    fn new(
        params: &[ValType],
        result: Option<ValType>,
        body: &FunctionBody<'_>,
    ) -> Result<Self, TranspileError> {
        let mut local_types = params.to_vec();
        for local in body.get_locals_reader()? {
            let (count, ty) = local?;
            for _ in 0..count {
                local_types.push(ty);
            }
        }

        let mutable_locals = collect_mutated_locals(body)?;

        let mut cur = Vec::new();
        // Declared locals default to zero; parameters arrive already bound.
        for (i, ty) in local_types.iter().enumerate().skip(params.len()) {
            let keyword = if mutable_locals.contains(&index_u32(i)?) {
                "let mut"
            } else {
                "let"
            };
            cur.push(format!(
                "{keyword} l{i}: {} = {};",
                rust_type(*ty)?,
                default_value(*ty)
            ));
        }

        Ok(Self {
            local_types,
            mutable_locals,
            result,
            stack: Vec::new(),
            frames: Vec::new(),
            cur,
            temp_counter: 0,
            label_counter: 0,
            reachable: true,
            dead_nesting: 0,
            trailing: None,
        })
    }

    fn run(&mut self, body: &FunctionBody<'_>) -> Result<(), TranspileError> {
        for op in body.get_operators_reader()? {
            self.emit_op(op?)?;
        }
        Ok(())
    }

    fn emit_op(&mut self, op: Operator<'_>) -> Result<(), TranspileError> {
        if !self.reachable {
            return self.skip_dead(&op);
        }

        match op {
            Operator::Nop => {}
            Operator::LocalGet { local_index } => {
                let ty = self.local_ty(local_index)?;
                let stable = !self.mutable_locals.contains(&local_index);
                self.push(Val {
                    code: format!("l{local_index}"),
                    ty,
                    stable,
                });
            }
            Operator::LocalSet { local_index } => self.local_store(local_index, false)?,
            Operator::LocalTee { local_index } => self.local_store(local_index, true)?,
            Operator::I32Const { value } => self.push(Val {
                code: i32_literal(value),
                ty: ValType::I32,
                stable: true,
            }),
            Operator::I32Add => self.binop_method("wrapping_add")?,
            Operator::I32Sub => self.binop_method("wrapping_sub")?,
            Operator::I32Mul => self.binop_method("wrapping_mul")?,
            Operator::I32And => self.binop_infix("&")?,
            Operator::I32Or => self.binop_infix("|")?,
            Operator::I32Xor => self.binop_infix("^")?,
            Operator::I32Eqz => self.compare_zero()?,
            Operator::I32Eq => self.compare_signed("==")?,
            Operator::I32Ne => self.compare_signed("!=")?,
            Operator::I32LtS => self.compare_signed("<")?,
            Operator::I32GtS => self.compare_signed(">")?,
            Operator::I32LeS => self.compare_signed("<=")?,
            Operator::I32GeS => self.compare_signed(">=")?,
            Operator::I32LtU => self.compare_unsigned("<")?,
            Operator::I32GtU => self.compare_unsigned(">")?,
            Operator::I32LeU => self.compare_unsigned("<=")?,
            Operator::I32GeU => self.compare_unsigned(">=")?,
            Operator::Drop => {
                self.pop()?;
            }
            Operator::Select => self.select()?,
            Operator::TypedSelect { .. } => self.select()?,
            Operator::Block { blockty } => self.open_frame(FrameKind::Block, blockty)?,
            Operator::Loop { blockty } => self.open_frame(FrameKind::Loop, blockty)?,
            Operator::If { blockty } => self.open_if(blockty)?,
            Operator::Else => self.handle_else()?,
            Operator::End => self.handle_end()?,
            Operator::Br { relative_depth } => self.branch(relative_depth, None)?,
            Operator::BrIf { relative_depth } => {
                let cond = self.pop()?;
                self.branch(relative_depth, Some(cond))?;
            }
            Operator::BrTable { targets } => self.branch_table(targets)?,
            Operator::Return => self.emit_return()?,
            other => {
                return Err(TranspileError::Unsupported(format!("operator {other:?}")));
            }
        }
        Ok(())
    }

    /// Skip an operator that appears in unreachable code, tracking region
    /// nesting so the matching `end`/`else` of the live frame is still handled.
    fn skip_dead(&mut self, op: &Operator<'_>) -> Result<(), TranspileError> {
        match op {
            Operator::Block { .. } | Operator::Loop { .. } | Operator::If { .. } => {
                self.dead_nesting += 1;
            }
            Operator::End => {
                if self.dead_nesting > 0 {
                    self.dead_nesting -= 1;
                } else {
                    self.handle_end()?;
                }
            }
            Operator::Else if self.dead_nesting == 0 => {
                self.handle_else()?;
            }
            _ => {}
        }
        Ok(())
    }

    // ----- operand stack helpers -------------------------------------------

    fn push(&mut self, val: Val) {
        self.stack.push(val);
    }

    fn pop(&mut self) -> Result<Val, TranspileError> {
        self.stack.pop().ok_or(TranspileError::StackUnderflow)
    }

    fn line(&mut self, text: impl Into<String>) {
        self.cur.push(text.into());
    }

    fn fresh_temp(&mut self) -> String {
        let name = format!("v{}", self.temp_counter);
        self.temp_counter += 1;
        name
    }

    /// Materialise every non-stable operand into a `let` temporary so its value
    /// is fixed across an upcoming control-flow boundary or local mutation.
    fn spill_nonstable(&mut self) -> Result<(), TranspileError> {
        for i in 0..self.stack.len() {
            if self.stack[i].stable {
                continue;
            }
            let ty = self.stack[i].ty;
            let code = self.stack[i].code.clone();
            let name = self.fresh_temp();
            self.line(format!("let {name}: {} = {code};", rust_type(ty)?));
            self.stack[i] = Val {
                code: name,
                ty,
                stable: true,
            };
        }
        Ok(())
    }

    // ----- numeric operators -----------------------------------------------

    fn binop_method(&mut self, method: &str) -> Result<(), TranspileError> {
        let rhs = self.pop()?;
        let lhs = self.pop()?;
        self.push(Val {
            code: format!("{}.{method}({})", lhs.code, rhs.code),
            ty: ValType::I32,
            stable: lhs.stable && rhs.stable,
        });
        Ok(())
    }

    fn binop_infix(&mut self, op: &str) -> Result<(), TranspileError> {
        let rhs = self.pop()?;
        let lhs = self.pop()?;
        self.push(Val {
            code: format!("({} {op} {})", lhs.code, rhs.code),
            ty: ValType::I32,
            stable: lhs.stable && rhs.stable,
        });
        Ok(())
    }

    fn compare_zero(&mut self) -> Result<(), TranspileError> {
        let a = self.pop()?;
        self.push(Val {
            code: format!("i32::from({} == 0)", a.code),
            ty: ValType::I32,
            stable: a.stable,
        });
        Ok(())
    }

    fn compare_signed(&mut self, op: &str) -> Result<(), TranspileError> {
        let rhs = self.pop()?;
        let lhs = self.pop()?;
        self.push(Val {
            code: format!("i32::from({} {op} {})", lhs.code, rhs.code),
            ty: ValType::I32,
            stable: lhs.stable && rhs.stable,
        });
        Ok(())
    }

    fn compare_unsigned(&mut self, op: &str) -> Result<(), TranspileError> {
        let rhs = self.pop()?;
        let lhs = self.pop()?;
        self.push(Val {
            code: format!(
                "i32::from(({} as u32) {op} ({} as u32))",
                lhs.code, rhs.code
            ),
            ty: ValType::I32,
            stable: lhs.stable && rhs.stable,
        });
        Ok(())
    }

    fn select(&mut self) -> Result<(), TranspileError> {
        let cond = self.pop()?;
        let b = self.pop()?;
        let a = self.pop()?;
        // Parenthesised so the `if` expression composes safely when this value
        // is later embedded in a larger expression (e.g. as an operator arm).
        self.push(Val {
            code: format!(
                "(if {} != 0 {{ {} }} else {{ {} }})",
                cond.code, a.code, b.code
            ),
            ty: a.ty,
            stable: cond.stable && a.stable && b.stable,
        });
        Ok(())
    }

    // ----- locals ----------------------------------------------------------

    fn local_store(&mut self, local_index: u32, keep: bool) -> Result<(), TranspileError> {
        // Fix any operand that reads a mutable local before we overwrite it.
        self.spill_nonstable()?;
        let value = if keep {
            self.stack
                .last()
                .cloned()
                .ok_or(TranspileError::StackUnderflow)?
        } else {
            self.pop()?
        };
        self.line(format!("l{local_index} = {};", value.code));
        Ok(())
    }

    fn local_ty(&self, local_index: u32) -> Result<ValType, TranspileError> {
        self.local_types
            .get(local_index as usize)
            .copied()
            .ok_or_else(|| TranspileError::Unsupported("local index out of range".into()))
    }

    // ----- control flow ----------------------------------------------------

    fn frame_result(
        &mut self,
        blockty: BlockType,
    ) -> Result<Option<(String, ValType)>, TranspileError> {
        match blockty {
            BlockType::Empty => Ok(None),
            BlockType::Type(ty) => {
                let name = self.fresh_temp();
                // Initialise to a default so every path is definitely assigned.
                self.line(format!(
                    "let mut {name}: {} = {};",
                    rust_type(ty)?,
                    default_value(ty)
                ));
                Ok(Some((name, ty)))
            }
            BlockType::FuncType(_) => {
                Err(TranspileError::Unsupported("block with parameters".into()))
            }
        }
    }

    fn open_frame(&mut self, kind: FrameKind, blockty: BlockType) -> Result<(), TranspileError> {
        self.spill_nonstable()?;
        let result = self.frame_result(blockty)?;
        let label = self.label_counter;
        self.label_counter += 1;
        let parent_height = self.stack.len();
        let parent_buffer = mem::take(&mut self.cur);
        self.frames.push(Frame {
            kind,
            label,
            targeted: false,
            result,
            parent_height,
            parent_buffer,
            then_buffer: None,
            then_reachable: false,
            cond: None,
        });
        Ok(())
    }

    fn open_if(&mut self, blockty: BlockType) -> Result<(), TranspileError> {
        let cond = self.pop()?;
        self.spill_nonstable()?;
        let result = self.frame_result(blockty)?;
        let label = self.label_counter;
        self.label_counter += 1;
        let parent_height = self.stack.len();
        let parent_buffer = mem::take(&mut self.cur);
        self.frames.push(Frame {
            kind: FrameKind::If,
            label,
            targeted: false,
            result,
            parent_height,
            parent_buffer,
            then_buffer: None,
            then_reachable: false,
            cond: Some(format!("{} != 0", cond.code)),
        });
        Ok(())
    }

    /// Assign the fall-through result of the current frame, if it has one.
    fn assign_fallthrough_result(&mut self) -> Result<(), TranspileError> {
        let target = self.frames.last().and_then(|f| f.result.clone());
        if let Some((var, _)) = target {
            let value = self.pop()?;
            self.line(format!("{var} = {};", value.code));
        }
        Ok(())
    }

    fn handle_else(&mut self) -> Result<(), TranspileError> {
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
        self.stack.truncate(parent_height);
        self.reachable = true;
        Ok(())
    }

    fn handle_end(&mut self) -> Result<(), TranspileError> {
        let Some(frame) = self.frames.pop() else {
            return self.end_function();
        };

        // Fall-through result assignment (for blocks/loops and the else arm).
        if let (true, Some((var, _))) = (self.reachable, &frame.result) {
            let value = self.pop()?;
            let var = var.clone();
            self.line(format!("{var} = {};", value.code));
        }

        let body = mem::take(&mut self.cur);
        let reachable_at_end = self.reachable;
        let rendered = self.render_frame(&frame, body, reachable_at_end)?;
        let next_reachable = reachable_after(&frame, reachable_at_end);

        self.cur = frame.parent_buffer;
        self.cur.extend(rendered);

        self.stack.truncate(frame.parent_height);
        if let Some((var, ty)) = frame.result {
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
    fn render_frame(
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

    fn branch(&mut self, depth: u32, cond: Option<Val>) -> Result<(), TranspileError> {
        let idx = self
            .frames
            .len()
            .checked_sub(1 + depth as usize)
            .ok_or_else(|| TranspileError::Unsupported("branch depth out of range".into()))?;
        let is_loop = self.frames[idx].kind == FrameKind::Loop;
        let label = self.frames[idx].label;
        // Branching to a loop targets its (empty) parameters; branching to a
        // block or if carries the region's result value.
        let result = if is_loop {
            None
        } else {
            self.frames[idx].result.clone()
        };
        self.frames[idx].targeted = true;

        let keyword = if is_loop { "continue" } else { "break" };
        match cond {
            None => {
                if let Some((var, _)) = result {
                    let value = self.pop()?;
                    self.line(format!("{var} = {};", value.code));
                }
                self.line(format!("{keyword} 'l{label};"));
                self.reachable = false;
                self.dead_nesting = 0;
            }
            Some(cond) => {
                if let Some((var, _)) = result {
                    // The result value stays on the stack for the fall-through
                    // path, so materialise it and reference the temporary.
                    self.spill_nonstable()?;
                    let value = self
                        .stack
                        .last()
                        .cloned()
                        .ok_or(TranspileError::StackUnderflow)?;
                    self.line(format!("if {} != 0 {{", cond.code));
                    self.line(format!("    {var} = {};", value.code));
                    self.line(format!("    {keyword} 'l{label};"));
                    self.line("}".to_string());
                } else {
                    self.line(format!("if {} != 0 {{ {keyword} 'l{label}; }}", cond.code));
                }
            }
        }
        Ok(())
    }

    fn branch_table(&mut self, targets: wasmparser::BrTable<'_>) -> Result<(), TranspileError> {
        let selector = self.pop()?;
        self.spill_nonstable()?;

        let default = targets.default();
        let mut arms: Vec<(Option<u32>, u32)> = Vec::new();
        for (i, target) in targets.targets().enumerate() {
            arms.push((Some(index_u32(i)?), target?));
        }
        arms.push((None, default));

        // The `br_table` selector is interpreted as an unsigned index.
        self.line(format!("match ({}) as u32 {{", selector.code));
        for (case, depth) in arms {
            let (keyword, label) = self.branch_arm(depth)?;
            let pattern = match case {
                Some(n) => format!("{n}u32"),
                None => "_".to_string(),
            };
            self.line(format!("    {pattern} => {keyword} 'l{label},"));
        }
        self.line("}".to_string());
        self.reachable = false;
        self.dead_nesting = 0;
        Ok(())
    }

    /// Resolve a branch-table target depth to a `(keyword, label)` pair and mark
    /// the target frame as branched to. Result-carrying targets are rejected in
    /// a `br_table` for now, since each arm would need its own assignment.
    fn branch_arm(&mut self, depth: u32) -> Result<(&'static str, usize), TranspileError> {
        let idx = self
            .frames
            .len()
            .checked_sub(1 + depth as usize)
            .ok_or_else(|| TranspileError::Unsupported("branch depth out of range".into()))?;
        let frame = &self.frames[idx];
        if frame.kind != FrameKind::Loop && frame.result.is_some() {
            return Err(TranspileError::Unsupported(
                "br_table targeting a block with a result".into(),
            ));
        }
        let keyword = if frame.kind == FrameKind::Loop {
            "continue"
        } else {
            "break"
        };
        let label = frame.label;
        self.frames[idx].targeted = true;
        Ok((keyword, label))
    }

    fn emit_return(&mut self) -> Result<(), TranspileError> {
        match self.result {
            Some(_) => {
                let value = self.pop()?;
                self.line(format!("return {};", value.code));
            }
            None => self.line("return;".to_string()),
        }
        self.reachable = false;
        self.dead_nesting = 0;
        Ok(())
    }

    fn end_function(&mut self) -> Result<(), TranspileError> {
        match (self.result, self.reachable) {
            (Some(_), true) => {
                let value = self.pop()?;
                self.trailing = Some(value.code);
            }
            (Some(_), false) => {
                // Control never falls off the end; a `return`/`br` already
                // produced the value, so no trailing expression is needed.
            }
            (None, _) => {}
        }
        Ok(())
    }

    fn finish(
        self,
        index: usize,
        params: &[ValType],
        result: Option<ValType>,
    ) -> Result<String, TranspileError> {
        let mut params_src = String::new();
        for (i, ty) in params.iter().enumerate() {
            if i > 0 {
                params_src.push_str(", ");
            }
            let keyword = if self.mutable_locals.contains(&index_u32(i)?) {
                "mut "
            } else {
                ""
            };
            params_src.push_str(&format!("{keyword}l{i}: {}", rust_type(*ty)?));
        }

        let ret = match result {
            Some(ty) => format!(" -> {}", rust_type(ty)?),
            None => String::new(),
        };

        let mut body = self.cur;
        if let Some(trailing) = self.trailing {
            body.push(trailing);
        }

        // wasm functions may leave parameters or locals unused, and result
        // temporaries are default-initialised before being assigned on every
        // exit path; both are inherent to the translation rather than bugs, so
        // suppress the lints they would otherwise trigger in the output.
        let mut out = String::from(
            "#[allow(unused_variables, unused_assignments, unused_mut, unused_parens)]\n",
        );
        out.push_str(&format!("pub fn func{index}({params_src}){ret} {{\n"));
        for line in indent(&body) {
            out.push_str(&line);
            out.push('\n');
        }
        out.push_str("}\n");
        Ok(out)
    }
}

/// Indent each non-empty line by four spaces.
fn indent(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .map(|l| {
            if l.is_empty() {
                String::new()
            } else {
                format!("    {l}")
            }
        })
        .collect()
}

/// Collect the indices of locals written by `local.set`/`local.tee`.
fn collect_mutated_locals(body: &FunctionBody<'_>) -> Result<HashSet<u32>, TranspileError> {
    let mut mutated = HashSet::new();
    for op in body.get_operators_reader()? {
        match op? {
            Operator::LocalSet { local_index } | Operator::LocalTee { local_index } => {
                mutated.insert(local_index);
            }
            _ => {}
        }
    }
    Ok(mutated)
}

/// Whether control can reach the code following a finished frame.
fn reachable_after(frame: &Frame, reachable_at_end: bool) -> bool {
    match frame.kind {
        // A loop is only exited by falling through its end.
        FrameKind::Loop => reachable_at_end,
        // A block/if is exited by fall-through or by a `br` that targets it. An
        // `if` without an `else` always has the condition-false fall-through.
        FrameKind::Block => reachable_at_end || frame.targeted,
        FrameKind::If => {
            if frame.then_buffer.is_none() {
                return true;
            }
            reachable_at_end || frame.then_reachable || frame.targeted
        }
    }
}

fn rust_type(ty: ValType) -> Result<&'static str, TranspileError> {
    match ty {
        ValType::I32 => Ok("i32"),
        ValType::I64 => Ok("i64"),
        ValType::F32 => Ok("f32"),
        ValType::F64 => Ok("f64"),
        other => Err(TranspileError::Unsupported(format!("value type {other:?}"))),
    }
}

fn default_value(ty: ValType) -> &'static str {
    match ty {
        ValType::F32 | ValType::F64 => "0.0",
        _ => "0",
    }
}

/// Render an `i32` constant as a valid Rust expression. `i32::MIN` cannot be
/// written as the literal `-2147483648i32` (Rust parses that as negation of the
/// out-of-range literal `2147483648i32`), so it uses the associated constant.
fn i32_literal(value: i32) -> String {
    if value == i32::MIN {
        "i32::MIN".to_string()
    } else {
        format!("{value}i32")
    }
}

fn index_u32(i: usize) -> Result<u32, TranspileError> {
    u32::try_from(i).map_err(|_| TranspileError::Unsupported("index too large".into()))
}
