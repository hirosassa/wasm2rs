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

use wasmparser::{BlockType, ConstExpr, FunctionBody, MemArg, Operator, ValType};

use crate::TranspileError;

/// Metadata about a module global.
pub(crate) struct GlobalInfo {
    pub ty: ValType,
    pub mutable: bool,
    /// The Rust expression that produces the global's initial value.
    pub init: String,
}

/// Metadata about the module's linear memory. The declared maximum is not
/// tracked; `memory.grow` only enforces the wasm32 hard cap of 65536 pages.
pub(crate) struct MemInfo {
    pub min_pages: u64,
}

/// Metadata about the module's function table (a single `funcref` table). The
/// declared maximum is not tracked; growth beyond `min` is not supported yet.
pub(crate) struct TableInfo {
    pub min: u32,
}

/// One active element segment: function indices written into the table
/// starting at a constant `offset`.
pub(crate) struct ElemSegment {
    pub offset: u32,
    pub funcs: Vec<u32>,
}

/// One active data segment: raw bytes written into linear memory starting at a
/// constant `offset`.
pub(crate) struct DataSegment {
    pub offset: u32,
    pub bytes: Vec<u8>,
}

/// A function type from the type section: its parameter and result types. Used
/// to resolve a `call_indirect`'s declared type back to a signature.
pub(crate) struct TypeSig {
    pub params: Vec<ValType>,
    pub results: Vec<ValType>,
}

/// One function to translate: its signature plus its body.
pub(crate) struct FuncInput<'a> {
    pub params: &'a [ValType],
    pub results: &'a [ValType],
    pub body: &'a FunctionBody<'a>,
}

/// A memory-access helper method emitted on the instance `impl` on demand.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Helper {
    LoadI32,
    Load8U,
    Load8S,
    Load16U,
    Load16S,
    StoreI32,
    Store8,
    Store16,
    Grow,
}

/// The rendered source of one function plus the memory helpers it relies on.
struct GenFn {
    src: String,
    helpers: HashSet<Helper>,
}

/// The lint-suppression attribute prefixed to generated functions/impls.
const ALLOW: &str =
    "#[allow(dead_code, unused_variables, unused_assignments, unused_mut, unused_parens)]";

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

/// Module-wide context shared by every function's code generation.
struct ModuleCtx<'a> {
    /// Every function, indexed by function index, so a `call` can read its
    /// callee's signature (arity and result).
    funcs: &'a [FuncInput<'a>],
    /// Every function type, so `call_indirect` can resolve its declared type
    /// index back to a signature.
    types: &'a [TypeSig],
    /// Per-global `(type, mutable)`, indexed by global index.
    globals: Vec<(ValType, bool)>,
    /// Whether the module declares linear memory (so `self.memory` exists).
    has_memory: bool,
    /// Whether the module declares a table (so `self.table` exists).
    has_table: bool,
    /// Whether functions are emitted as `&mut self` methods (stateful module).
    is_method: bool,
}

/// Translate a whole module into Rust source.
///
/// A module that declares linear memory, a table or globals carries mutable
/// state, so it is emitted as a `pub struct Instance` with the functions as
/// `&mut self` methods. A stateless module keeps its functions as free
/// `pub fn`s, matching the earlier phases exactly.
pub(crate) fn generate_module(
    funcs: &[FuncInput<'_>],
    types: &[TypeSig],
    globals: &[GlobalInfo],
    memory: Option<&MemInfo>,
    data: &[DataSegment],
    table: Option<&TableInfo>,
    elements: &[ElemSegment],
) -> Result<String, TranspileError> {
    let has_memory = memory.is_some();
    let has_table = table.is_some();
    let stateful = has_memory || has_table || !globals.is_empty();

    let ctx = ModuleCtx {
        funcs,
        types,
        globals: globals.iter().map(|g| (g.ty, g.mutable)).collect(),
        has_memory,
        has_table,
        is_method: stateful,
    };

    let mut sources = Vec::with_capacity(funcs.len());
    let mut used: HashSet<Helper> = HashSet::new();
    for (index, f) in funcs.iter().enumerate() {
        let generated = generate_function(index, f, &ctx)?;
        used.extend(generated.helpers);
        sources.push(generated.src);
    }

    if !stateful {
        return Ok(sources.join("\n"));
    }
    render_module(&sources, globals, memory, data, table, elements, &used)
}

fn generate_function(
    index: usize,
    input: &FuncInput<'_>,
    ctx: &ModuleCtx<'_>,
) -> Result<GenFn, TranspileError> {
    let result = match input.results {
        [] => None,
        [ty] => Some(*ty),
        _ => return Err(TranspileError::Unsupported("multi-value results".into())),
    };

    let mut func = FuncGen::new(input.params, result, input.body, ctx)?;
    func.run(input.body)?;
    func.finish(index, input.params, result)
}

/// State threaded through the translation of a single function body.
struct FuncGen<'a> {
    local_types: Vec<ValType>,
    mutable_locals: HashSet<u32>,
    /// Module-wide context (functions, types, globals, stateful flags).
    ctx: &'a ModuleCtx<'a>,
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
    /// Memory-access helpers this function relies on.
    used_helpers: HashSet<Helper>,
}

impl<'a> FuncGen<'a> {
    fn new(
        params: &[ValType],
        result: Option<ValType>,
        body: &FunctionBody<'_>,
        ctx: &'a ModuleCtx<'a>,
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
            ctx,
            result,
            stack: Vec::new(),
            frames: Vec::new(),
            cur,
            temp_counter: 0,
            label_counter: 0,
            reachable: true,
            dead_nesting: 0,
            trailing: None,
            used_helpers: HashSet::new(),
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
            Operator::GlobalGet { global_index } => self.global_get(global_index)?,
            Operator::GlobalSet { global_index } => self.global_set(global_index)?,
            Operator::I32Load { memarg } => self.load(Helper::LoadI32, memarg)?,
            Operator::I32Load8U { memarg } => self.load(Helper::Load8U, memarg)?,
            Operator::I32Load8S { memarg } => self.load(Helper::Load8S, memarg)?,
            Operator::I32Load16U { memarg } => self.load(Helper::Load16U, memarg)?,
            Operator::I32Load16S { memarg } => self.load(Helper::Load16S, memarg)?,
            Operator::I32Store { memarg } => self.store(Helper::StoreI32, memarg)?,
            Operator::I32Store8 { memarg } => self.store(Helper::Store8, memarg)?,
            Operator::I32Store16 { memarg } => self.store(Helper::Store16, memarg)?,
            Operator::MemorySize { .. } => self.memory_size()?,
            Operator::MemoryGrow { .. } => self.memory_grow()?,
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
            Operator::Call { function_index } => self.call(function_index)?,
            Operator::CallIndirect {
                type_index,
                table_index,
            } => self.call_indirect(type_index, table_index)?,
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

    // ----- globals ---------------------------------------------------------

    fn global(&self, global_index: u32) -> Result<(ValType, bool), TranspileError> {
        self.ctx
            .globals
            .get(global_index as usize)
            .copied()
            .ok_or_else(|| TranspileError::Unsupported("global index out of range".into()))
    }

    fn global_get(&mut self, global_index: u32) -> Result<(), TranspileError> {
        let (ty, mutable) = self.global(global_index)?;
        self.push(Val {
            code: format!("self.g{global_index}"),
            ty,
            // A mutable global can be changed by a later `global.set`.
            stable: !mutable,
        });
        Ok(())
    }

    fn global_set(&mut self, global_index: u32) -> Result<(), TranspileError> {
        let (_, mutable) = self.global(global_index)?;
        if !mutable {
            return Err(TranspileError::Unsupported(
                "set of immutable global".into(),
            ));
        }
        self.spill_nonstable()?;
        let value = self.pop()?;
        self.line(format!("self.g{global_index} = {};", value.code));
        Ok(())
    }

    // ----- linear memory ---------------------------------------------------

    fn require_memory(&self) -> Result<(), TranspileError> {
        if self.ctx.has_memory {
            Ok(())
        } else {
            Err(TranspileError::Unsupported(
                "memory instruction without a memory section".into(),
            ))
        }
    }

    fn load(&mut self, helper: Helper, memarg: MemArg) -> Result<(), TranspileError> {
        self.require_memory()?;
        let offset = memarg_offset(memarg)?;
        let addr = self.pop()?;
        self.used_helpers.insert(helper);
        self.push(Val {
            code: format!(
                "self.{}(({}) as u32, {offset}u32)",
                helper_name(helper),
                addr.code
            ),
            ty: ValType::I32,
            // The result depends on memory contents, which a store can change.
            stable: false,
        });
        Ok(())
    }

    fn store(&mut self, helper: Helper, memarg: MemArg) -> Result<(), TranspileError> {
        self.require_memory()?;
        let offset = memarg_offset(memarg)?;
        // Memory is about to change; fix any operand that reads from it.
        self.spill_nonstable()?;
        let value = self.pop()?;
        let addr = self.pop()?;
        self.used_helpers.insert(helper);
        self.line(format!(
            "self.{}(({}) as u32, {offset}u32, {});",
            helper_name(helper),
            addr.code,
            value.code
        ));
        Ok(())
    }

    fn memory_size(&mut self) -> Result<(), TranspileError> {
        self.require_memory()?;
        self.push(Val {
            code: "((self.memory.len() / 65536) as i32)".to_string(),
            ty: ValType::I32,
            // `memory.grow` can change the size.
            stable: false,
        });
        Ok(())
    }

    fn memory_grow(&mut self) -> Result<(), TranspileError> {
        self.require_memory()?;
        self.spill_nonstable()?;
        let delta = self.pop()?;
        self.used_helpers.insert(Helper::Grow);
        let name = self.fresh_temp();
        self.line(format!(
            "let {name}: i32 = self.memory_grow({});",
            delta.code
        ));
        self.push(Val {
            code: name,
            ty: ValType::I32,
            stable: true,
        });
        Ok(())
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
        self.push_frame(kind, blockty, None)
    }

    fn open_if(&mut self, blockty: BlockType) -> Result<(), TranspileError> {
        // The condition is consumed before the surrounding stack is spilled, so
        // it is popped here rather than inside `push_frame`.
        let cond = self.pop()?;
        self.push_frame(FrameKind::If, blockty, Some(format!("{} != 0", cond.code)))
    }

    /// Spill the operand stack, allocate a label, and push a fresh frame that
    /// captures the enclosing scope's height and output buffer.
    fn push_frame(
        &mut self,
        kind: FrameKind,
        blockty: BlockType,
        cond: Option<String>,
    ) -> Result<(), TranspileError> {
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
            cond,
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

    // ----- calls -----------------------------------------------------------

    fn call(&mut self, function_index: u32) -> Result<(), TranspileError> {
        let callee = self
            .ctx
            .funcs
            .get(function_index as usize)
            .ok_or_else(|| TranspileError::Unsupported("call to unknown function".into()))?;
        let param_count = callee.params.len();
        let result = match callee.results {
            [] => None,
            [ty] => Some(*ty),
            _ => {
                return Err(TranspileError::Unsupported(
                    "multi-value call result".into(),
                ));
            }
        };

        // A call may read and write memory and globals. Freezing every operand
        // first both materialises the arguments and pins any earlier value that
        // must not observe the call's side effects (spill-before-mutation).
        self.spill_nonstable()?;

        let mut args = Vec::with_capacity(param_count);
        for _ in 0..param_count {
            args.push(self.pop()?);
        }
        args.reverse();
        let arg_list = args
            .into_iter()
            .map(|a| a.code)
            .collect::<Vec<_>>()
            .join(", ");

        let receiver = if self.ctx.is_method { "self." } else { "" };
        let call_expr = format!("{receiver}func{function_index}({arg_list})");
        match result {
            // A call is not re-evaluatable, so bind it to a temporary at exactly
            // this point (mirroring `memory_grow`) and push the stable temp.
            Some(ty) => {
                let name = self.fresh_temp();
                self.line(format!("let {name}: {} = {call_expr};", rust_type(ty)?));
                self.push(Val {
                    code: name,
                    ty,
                    stable: true,
                });
            }
            None => self.line(format!("{call_expr};")),
        }
        Ok(())
    }

    fn call_indirect(&mut self, type_index: u32, table_index: u32) -> Result<(), TranspileError> {
        if !self.ctx.has_table {
            return Err(TranspileError::Unsupported(
                "call_indirect without a table".into(),
            ));
        }
        if table_index != 0 {
            return Err(TranspileError::Unsupported(
                "call_indirect on a non-zero table".into(),
            ));
        }
        let sig = self
            .ctx
            .types
            .get(type_index as usize)
            .ok_or_else(|| TranspileError::Unsupported("call_indirect: unknown type".into()))?;
        let result = match sig.results.as_slice() {
            [] => None,
            [ty] => Some(*ty),
            _ => {
                return Err(TranspileError::Unsupported(
                    "multi-value call_indirect result".into(),
                ));
            }
        };
        // The functions any table entry could resolve to: exactly those whose
        // signature equals the declared type (no subtyping, so a structural
        // match is a type match).
        let targets: Vec<usize> = self
            .ctx
            .funcs
            .iter()
            .enumerate()
            .filter(|(_, f)| {
                f.params == sig.params.as_slice() && f.results == sig.results.as_slice()
            })
            .map(|(i, _)| i)
            .collect();

        // Freeze operands (arguments and the table index) before the call, both
        // to share them across every match arm and to pin any earlier value
        // against the callee's side effects (spill-before-mutation).
        self.spill_nonstable()?;
        let index = self.pop()?;
        let mut args = Vec::with_capacity(sig.params.len());
        for _ in 0..sig.params.len() {
            args.push(self.pop()?);
        }
        args.reverse();
        let arg_list = args
            .into_iter()
            .map(|a| a.code)
            .collect::<Vec<_>>()
            .join(", ");

        // No function has the requested type, so every entry mismatches: the
        // call always traps and control cannot continue past it.
        if targets.is_empty() {
            self.line("panic!(\"indirect call type mismatch\");".to_string());
            self.reachable = false;
            self.dead_nesting = 0;
            return Ok(());
        }

        // A `match` on the table entry, emitted line by line so `indent` aligns
        // it. When the call yields a value the opening line binds a temporary;
        // otherwise the `match` stands alone as a statement.
        //
        // An out-of-bounds index panics on the slice access (a trap); a null or
        // wrong-type entry falls through to the catch-all panic (also a trap).
        let result = match result {
            Some(ty) => {
                let name = self.fresh_temp();
                self.line(format!(
                    "let {name}: {} = match self.table[({}) as u32 as usize] {{",
                    rust_type(ty)?,
                    index.code
                ));
                Some((name, ty))
            }
            None => {
                self.line(format!(
                    "match self.table[({}) as u32 as usize] {{",
                    index.code
                ));
                None
            }
        };
        for i in &targets {
            self.line(format!("    {i}u32 => self.func{i}({arg_list}),"));
        }
        self.line("    _ => panic!(\"indirect call type mismatch\"),".to_string());
        self.line("};".to_string());

        if let Some((name, ty)) = result {
            self.push(Val {
                code: name,
                ty,
                stable: true,
            });
        }
        Ok(())
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
    ) -> Result<GenFn, TranspileError> {
        let mut params_src = String::new();
        // Stateful modules pass their memory/globals through `&mut self`.
        if self.ctx.is_method {
            params_src.push_str("&mut self");
        }
        for (i, ty) in params.iter().enumerate() {
            if self.ctx.is_method || i > 0 {
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

        // For a method the lint-suppression attribute is applied once on the
        // enclosing `impl`; free functions carry it individually.
        let mut out = String::new();
        if !self.ctx.is_method {
            out.push_str(ALLOW);
            out.push('\n');
        }
        out.push_str(&format!("pub fn func{index}({params_src}){ret} {{\n"));
        for line in indent(&body) {
            out.push_str(&line);
            out.push('\n');
        }
        out.push_str("}\n");
        Ok(GenFn {
            src: out,
            helpers: self.used_helpers,
        })
    }
}

/// The offset field of a memory access, as a `u32` (32-bit memory only).
fn memarg_offset(memarg: MemArg) -> Result<u32, TranspileError> {
    u32::try_from(memarg.offset)
        .map_err(|_| TranspileError::Unsupported("memory offset too large".into()))
}

fn helper_name(helper: Helper) -> &'static str {
    match helper {
        Helper::LoadI32 => "load_i32",
        Helper::Load8U => "load8_u",
        Helper::Load8S => "load8_s",
        Helper::Load16U => "load16_u",
        Helper::Load16S => "load16_s",
        Helper::StoreI32 => "store_i32",
        Helper::Store8 => "store8",
        Helper::Store16 => "store16",
        Helper::Grow => "memory_grow",
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

fn i64_literal(value: i64) -> String {
    if value == i64::MIN {
        "i64::MIN".to_string()
    } else {
        format!("{value}i64")
    }
}

/// Evaluate a constant offset expression (a single `i32.const`) to a `u32`,
/// as used for an active element segment's table offset.
pub(crate) fn const_expr_u32(expr: &ConstExpr<'_>) -> Result<u32, TranspileError> {
    let mut value: Option<u32> = None;
    for op in expr.get_operators_reader() {
        match op? {
            Operator::I32Const { value: v } => {
                value =
                    Some(u32::try_from(v).map_err(|_| {
                        TranspileError::Unsupported("negative table offset".into())
                    })?);
            }
            Operator::End => {}
            other => {
                return Err(TranspileError::Unsupported(format!(
                    "element offset: {other:?}"
                )));
            }
        }
    }
    value.ok_or_else(|| TranspileError::Unsupported("empty element offset".into()))
}

/// Translate a global's constant initializer expression to a Rust expression.
pub(crate) fn const_expr_to_rust(expr: &ConstExpr<'_>) -> Result<String, TranspileError> {
    let mut value: Option<String> = None;
    for op in expr.get_operators_reader() {
        match op? {
            Operator::I32Const { value: v } => value = Some(i32_literal(v)),
            Operator::I64Const { value: v } => value = Some(i64_literal(v)),
            Operator::F32Const { value: v } => {
                value = Some(format!("f32::from_bits({}u32)", v.bits()));
            }
            Operator::F64Const { value: v } => {
                value = Some(format!("f64::from_bits({}u64)", v.bits()));
            }
            Operator::End => {}
            other => {
                return Err(TranspileError::Unsupported(format!(
                    "global initializer: {other:?}"
                )));
            }
        }
    }
    value.ok_or_else(|| TranspileError::Unsupported("empty global initializer".into()))
}

/// All memory helpers, in a deterministic emission order.
const HELPER_ORDER: [Helper; 9] = [
    Helper::LoadI32,
    Helper::Load8U,
    Helper::Load8S,
    Helper::Load16U,
    Helper::Load16S,
    Helper::StoreI32,
    Helper::Store8,
    Helper::Store16,
    Helper::Grow,
];

/// Render the `struct Instance` and its `impl` for a stateful module.
fn render_module(
    sources: &[String],
    globals: &[GlobalInfo],
    memory: Option<&MemInfo>,
    data: &[DataSegment],
    table: Option<&TableInfo>,
    elements: &[ElemSegment],
    used: &HashSet<Helper>,
) -> Result<String, TranspileError> {
    let mut lines: Vec<String> = Vec::new();

    lines.push("#[allow(dead_code)]".to_string());
    lines.push("pub struct Instance {".to_string());
    if memory.is_some() {
        lines.push("    memory: Vec<u8>,".to_string());
    }
    if table.is_some() {
        // A table entry is a function index; `u32::MAX` marks a null funcref.
        lines.push("    table: Vec<u32>,".to_string());
    }
    for (i, g) in globals.iter().enumerate() {
        lines.push(format!("    g{i}: {},", rust_type(g.ty)?));
    }
    lines.push("}".to_string());
    lines.push(String::new());

    lines.push(ALLOW.to_string());
    lines.push("impl Instance {".to_string());

    // Everything inside the `impl` is collected unindented, then indented once.
    let mut inner: Vec<String> = Vec::new();
    inner.push("pub fn new() -> Self {".to_string());
    inner.push("    Self {".to_string());
    if let Some(m) = memory {
        let bytes = m
            .min_pages
            .checked_mul(65536)
            .ok_or_else(|| TranspileError::Unsupported("memory too large".into()))?;
        if data.is_empty() {
            inner.push(format!("        memory: vec![0u8; {bytes}],"));
        } else {
            // Zero the memory, then copy each active data segment into place.
            inner.push("        memory: {".to_string());
            inner.push(format!(
                "            let mut m: Vec<u8> = vec![0u8; {bytes}];"
            ));
            for seg in data {
                let off = seg.offset as usize;
                let end = off + seg.bytes.len();
                let bytes_lit = seg
                    .bytes
                    .iter()
                    .map(|b| format!("{b}u8"))
                    .collect::<Vec<_>>()
                    .join(", ");
                inner.push(format!(
                    "            m[{off}..{end}].copy_from_slice(&[{bytes_lit}]);"
                ));
            }
            inner.push("            m".to_string());
            inner.push("        },".to_string());
        }
    }
    if let Some(t) = table {
        if elements.is_empty() {
            inner.push(format!("        table: vec![u32::MAX; {}],", t.min));
        } else {
            // Start every slot null, then apply each active element segment.
            inner.push("        table: {".to_string());
            inner.push(format!(
                "            let mut t: Vec<u32> = vec![u32::MAX; {}];",
                t.min
            ));
            for seg in elements {
                for (k, f) in seg.funcs.iter().enumerate() {
                    let idx = seg.offset as usize + k;
                    inner.push(format!("            t[{idx}] = {f}u32;"));
                }
            }
            inner.push("            t".to_string());
            inner.push("        },".to_string());
        }
    }
    for (i, g) in globals.iter().enumerate() {
        inner.push(format!("        g{i}: {},", g.init));
    }
    inner.push("    }".to_string());
    inner.push("}".to_string());

    for helper in HELPER_ORDER {
        if used.contains(&helper) {
            inner.push(String::new());
            inner.extend(helper_lines(helper));
        }
    }

    for src in sources {
        inner.push(String::new());
        for line in src.lines() {
            inner.push(line.to_string());
        }
    }

    for line in indent(&inner) {
        lines.push(line);
    }
    lines.push("}".to_string());

    let mut out = lines.join("\n");
    out.push('\n');
    Ok(out)
}

/// The source lines of one memory helper method (bounds-checked via indexing,
/// so an out-of-range access panics — mirroring a wasm trap).
fn helper_lines(helper: Helper) -> Vec<String> {
    let owned = |lines: &[&str]| lines.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    match helper {
        Helper::LoadI32 => owned(&[
            "fn load_i32(&self, addr: u32, offset: u32) -> i32 {",
            "    let a = addr as usize + offset as usize;",
            "    i32::from_le_bytes([self.memory[a], self.memory[a + 1], self.memory[a + 2], self.memory[a + 3]])",
            "}",
        ]),
        Helper::Load8U => owned(&[
            "fn load8_u(&self, addr: u32, offset: u32) -> i32 {",
            "    let a = addr as usize + offset as usize;",
            "    self.memory[a] as i32",
            "}",
        ]),
        Helper::Load8S => owned(&[
            "fn load8_s(&self, addr: u32, offset: u32) -> i32 {",
            "    let a = addr as usize + offset as usize;",
            "    self.memory[a] as i8 as i32",
            "}",
        ]),
        Helper::Load16U => owned(&[
            "fn load16_u(&self, addr: u32, offset: u32) -> i32 {",
            "    let a = addr as usize + offset as usize;",
            "    u16::from_le_bytes([self.memory[a], self.memory[a + 1]]) as i32",
            "}",
        ]),
        Helper::Load16S => owned(&[
            "fn load16_s(&self, addr: u32, offset: u32) -> i32 {",
            "    let a = addr as usize + offset as usize;",
            "    i16::from_le_bytes([self.memory[a], self.memory[a + 1]]) as i32",
            "}",
        ]),
        Helper::StoreI32 => owned(&[
            "fn store_i32(&mut self, addr: u32, offset: u32, value: i32) {",
            "    let a = addr as usize + offset as usize;",
            "    let b = value.to_le_bytes();",
            "    self.memory[a] = b[0];",
            "    self.memory[a + 1] = b[1];",
            "    self.memory[a + 2] = b[2];",
            "    self.memory[a + 3] = b[3];",
            "}",
        ]),
        Helper::Store8 => owned(&[
            "fn store8(&mut self, addr: u32, offset: u32, value: i32) {",
            "    let a = addr as usize + offset as usize;",
            "    self.memory[a] = value as u8;",
            "}",
        ]),
        Helper::Store16 => owned(&[
            "fn store16(&mut self, addr: u32, offset: u32, value: i32) {",
            "    let a = addr as usize + offset as usize;",
            "    let b = (value as u16).to_le_bytes();",
            "    self.memory[a] = b[0];",
            "    self.memory[a + 1] = b[1];",
            "}",
        ]),
        // `delta` is an unsigned page count. Growth past the wasm32 limit of
        // 65536 pages (4 GiB) fails, returning -1 as the wasm spec requires;
        // the declared maximum is not tracked, so only that hard cap applies.
        Helper::Grow => owned(&[
            "fn memory_grow(&mut self, delta: i32) -> i32 {",
            "    let old_pages = (self.memory.len() / 65536) as u64;",
            "    let new_pages = old_pages + (delta as u32 as u64);",
            "    if new_pages > 65536 {",
            "        return -1;",
            "    }",
            "    self.memory.resize((new_pages as usize) * 65536, 0);",
            "    old_pages as i32",
            "}",
        ]),
    }
}
