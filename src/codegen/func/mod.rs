use std::collections::HashSet;

use wasmparser::{FunctionBody, Operator, ValType};

use super::{
    Frame, FrameKind, Helper, ModuleCtx, Node, Rt, Val, collect_mutated_locals, default_value,
    i32_literal, i64_literal, index_u32, rust_type,
};
use crate::TranspileError;

mod calls;
mod control;
mod memvals;
mod numeric;
mod table;

/// State threaded through the translation of a single function body.
pub(super) struct FuncGen<'a> {
    local_types: Vec<ValType>,
    mutable_locals: HashSet<u32>,
    /// Module-wide context (functions, types, globals, stateful flags).
    ctx: &'a ModuleCtx<'a>,
    /// The function's result types (0, 1 or more — a tuple when more than one).
    results: Vec<ValType>,
    stack: Vec<Val>,
    frames: Vec<Frame>,
    /// The deferred output of the innermost scope currently being emitted into.
    cur: Vec<Node>,
    temp_counter: usize,
    label_counter: usize,
    /// The deepest control-flow nesting reached (peak `frames.len()`), used to
    /// decide whether to flatten this function to bound its rendered nesting.
    max_depth: usize,
    /// Whether the current program point is reachable.
    reachable: bool,
    /// Nesting depth of regions opened while unreachable (skipped wholesale).
    dead_nesting: usize,
    /// The tail expression returned by the function, if any.
    trailing: Option<String>,
    /// Memory-access helpers this function relies on.
    used_helpers: HashSet<Helper>,
    /// Free-function runtime helpers this function relies on.
    used_rt: HashSet<Rt>,
    /// `call_indirect` type indices this function dispatches through; each needs
    /// a `call_ref_t{ti}` method on the instance.
    dispatch_sigs: HashSet<u32>,
}

impl<'a> FuncGen<'a> {
    pub(super) fn new(
        params: &[ValType],
        results: &[ValType],
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
            cur.push(Node::Line(format!(
                "{keyword} l{i}: {} = {};",
                rust_type(*ty)?,
                default_value(*ty)
            )));
        }

        Ok(Self {
            local_types,
            mutable_locals,
            ctx,
            results: results.to_vec(),
            stack: Vec::new(),
            frames: Vec::new(),
            cur,
            temp_counter: 0,
            label_counter: 0,
            max_depth: 0,
            reachable: true,
            dead_nesting: 0,
            trailing: None,
            used_helpers: HashSet::new(),
            used_rt: HashSet::new(),
            dispatch_sigs: HashSet::new(),
        })
    }

    pub(super) fn run(&mut self, body: &FunctionBody<'_>) -> Result<(), TranspileError> {
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
            // `unreachable` always traps; code after it is dead, so stop
            // emitting until the enclosing region ends (as for `return`/`br`).
            Operator::Unreachable => {
                self.term("panic!(\"unreachable\");".to_string());
                self.reachable = false;
                self.dead_nesting = 0;
            }
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
            // Division and remainder can trap, so they are materialised at this
            // program point (see `materialize`); shifts/rotates never trap.
            Operator::I32DivS => self.div_signed()?,
            Operator::I32DivU => self.div_rem_unsigned("/")?,
            Operator::I32RemS => self.rem_signed()?,
            Operator::I32RemU => self.div_rem_unsigned("%")?,
            Operator::I32Shl => self.shift_op("wrapping_shl")?,
            Operator::I32ShrS => self.shift_op("wrapping_shr")?,
            Operator::I32ShrU => self.unsigned_shift("wrapping_shr")?,
            Operator::I32Rotl => self.shift_op("rotate_left")?,
            Operator::I32Rotr => self.shift_op("rotate_right")?,
            Operator::I32Clz => self.bit_count(ValType::I32, "leading_zeros")?,
            Operator::I32Ctz => self.bit_count(ValType::I32, "trailing_zeros")?,
            Operator::I32Popcnt => self.bit_count(ValType::I32, "count_ones")?,
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
            Operator::I64Const { value } => self.push(Val {
                code: i64_literal(value),
                ty: ValType::I64,
                stable: true,
            }),
            Operator::I64Add => self.binop_method("wrapping_add")?,
            Operator::I64Sub => self.binop_method("wrapping_sub")?,
            Operator::I64Mul => self.binop_method("wrapping_mul")?,
            Operator::I64And => self.binop_infix("&")?,
            Operator::I64Or => self.binop_infix("|")?,
            Operator::I64Xor => self.binop_infix("^")?,
            Operator::I64DivS => self.div_signed()?,
            Operator::I64DivU => self.div_rem_unsigned("/")?,
            Operator::I64RemS => self.rem_signed()?,
            Operator::I64RemU => self.div_rem_unsigned("%")?,
            Operator::I64Shl => self.shift_op("wrapping_shl")?,
            Operator::I64ShrS => self.shift_op("wrapping_shr")?,
            Operator::I64ShrU => self.unsigned_shift("wrapping_shr")?,
            Operator::I64Rotl => self.shift_op("rotate_left")?,
            Operator::I64Rotr => self.shift_op("rotate_right")?,
            Operator::I64Clz => self.bit_count(ValType::I64, "leading_zeros")?,
            Operator::I64Ctz => self.bit_count(ValType::I64, "trailing_zeros")?,
            Operator::I64Popcnt => self.bit_count(ValType::I64, "count_ones")?,
            Operator::I64Eqz => self.compare_zero()?,
            Operator::I64Eq => self.compare_signed("==")?,
            Operator::I64Ne => self.compare_signed("!=")?,
            Operator::I64LtS => self.compare_signed("<")?,
            Operator::I64GtS => self.compare_signed(">")?,
            Operator::I64LeS => self.compare_signed("<=")?,
            Operator::I64GeS => self.compare_signed(">=")?,
            Operator::I64LtU => self.compare_unsigned("<")?,
            Operator::I64GtU => self.compare_unsigned(">")?,
            Operator::I64LeU => self.compare_unsigned("<=")?,
            Operator::I64GeU => self.compare_unsigned(">=")?,
            // Floats: constants are emitted from their exact bit pattern (so
            // NaN/inf round-trip); arithmetic and comparisons map to native
            // operators (Rust float compare yields false for NaN, as wasm
            // requires). `min`/`max` are deferred (special NaN semantics).
            Operator::F32Const { value } => self.push(Val {
                code: format!("f32::from_bits({}u32)", value.bits()),
                ty: ValType::F32,
                stable: true,
            }),
            Operator::F64Const { value } => self.push(Val {
                code: format!("f64::from_bits({}u64)", value.bits()),
                ty: ValType::F64,
                stable: true,
            }),
            Operator::F32Add | Operator::F64Add => self.binop_infix("+")?,
            Operator::F32Sub | Operator::F64Sub => self.binop_infix("-")?,
            Operator::F32Mul | Operator::F64Mul => self.binop_infix("*")?,
            Operator::F32Div | Operator::F64Div => self.binop_infix("/")?,
            Operator::F32Eq | Operator::F64Eq => self.compare_signed("==")?,
            Operator::F32Ne | Operator::F64Ne => self.compare_signed("!=")?,
            Operator::F32Lt | Operator::F64Lt => self.compare_signed("<")?,
            Operator::F32Gt | Operator::F64Gt => self.compare_signed(">")?,
            Operator::F32Le | Operator::F64Le => self.compare_signed("<=")?,
            Operator::F32Ge | Operator::F64Ge => self.compare_signed(">=")?,
            Operator::F32Abs | Operator::F64Abs => self.unop_method("abs")?,
            Operator::F32Neg | Operator::F64Neg => self.unop_neg()?,
            Operator::F32Ceil | Operator::F64Ceil => self.unop_method("ceil")?,
            Operator::F32Floor | Operator::F64Floor => self.unop_method("floor")?,
            Operator::F32Trunc | Operator::F64Trunc => self.unop_method("trunc")?,
            // wasm `nearest` rounds halves to even, i.e. `round_ties_even`.
            Operator::F32Nearest | Operator::F64Nearest => self.unop_method("round_ties_even")?,
            Operator::F32Sqrt | Operator::F64Sqrt => self.unop_method("sqrt")?,
            Operator::F32Copysign | Operator::F64Copysign => self.binop_method("copysign")?,
            // `min`/`max` differ from Rust's built-ins: wasm propagates NaN and
            // orders -0.0 below +0.0, so they route through runtime helpers.
            Operator::F32Min => self.call_rt_binop(Rt::F32Min)?,
            Operator::F32Max => self.call_rt_binop(Rt::F32Max)?,
            Operator::F64Min => self.call_rt_binop(Rt::F64Min)?,
            Operator::F64Max => self.call_rt_binop(Rt::F64Max)?,
            // Numeric conversions. Integer wrap/extend and int<->float casts map
            // to Rust `as` (which truncates integers and, for float->int, is
            // saturating — matching wasm's `trunc_sat`). `cast_as` is a single
            // `as R`; `cast_through` first reinterprets `as M` (an unsigned type
            // for unsigned variants, a narrower signed type for sign-extension).
            // `reinterpret` moves the bits unchanged.
            Operator::I32WrapI64 => self.cast_as(ValType::I32, "i32")?,
            Operator::I64ExtendI32S => self.cast_as(ValType::I64, "i64")?,
            Operator::I64ExtendI32U => self.cast_through(ValType::I64, "u32", "i64")?,
            Operator::I32Extend8S => self.cast_through(ValType::I32, "i8", "i32")?,
            Operator::I32Extend16S => self.cast_through(ValType::I32, "i16", "i32")?,
            Operator::I64Extend8S => self.cast_through(ValType::I64, "i8", "i64")?,
            Operator::I64Extend16S => self.cast_through(ValType::I64, "i16", "i64")?,
            Operator::I64Extend32S => self.cast_through(ValType::I64, "i32", "i64")?,
            Operator::F32ConvertI32S | Operator::F32ConvertI64S => {
                self.cast_as(ValType::F32, "f32")?
            }
            Operator::F64ConvertI32S | Operator::F64ConvertI64S => {
                self.cast_as(ValType::F64, "f64")?
            }
            Operator::F32ConvertI32U => self.cast_through(ValType::F32, "u32", "f32")?,
            Operator::F32ConvertI64U => self.cast_through(ValType::F32, "u64", "f32")?,
            Operator::F64ConvertI32U => self.cast_through(ValType::F64, "u32", "f64")?,
            Operator::F64ConvertI64U => self.cast_through(ValType::F64, "u64", "f64")?,
            Operator::F32DemoteF64 => self.cast_as(ValType::F32, "f32")?,
            Operator::F64PromoteF32 => self.cast_as(ValType::F64, "f64")?,
            Operator::I32ReinterpretF32 => self.reinterpret(ValType::I32, "i32")?,
            Operator::I64ReinterpretF64 => self.reinterpret(ValType::I64, "i64")?,
            Operator::F32ReinterpretI32 => {
                self.convert(ValType::F32, |x| format!("f32::from_bits({x} as u32)"))?
            }
            Operator::F64ReinterpretI64 => {
                self.convert(ValType::F64, |x| format!("f64::from_bits({x} as u64)"))?
            }
            // Non-saturating truncations trap on NaN or an out-of-range value,
            // so they route through runtime helpers (unlike the `_sat` casts).
            Operator::I32TruncF32S => self.call_rt_unop_trapping(Rt::I32TruncF32S, ValType::I32)?,
            Operator::I32TruncF32U => self.call_rt_unop_trapping(Rt::I32TruncF32U, ValType::I32)?,
            Operator::I32TruncF64S => self.call_rt_unop_trapping(Rt::I32TruncF64S, ValType::I32)?,
            Operator::I32TruncF64U => self.call_rt_unop_trapping(Rt::I32TruncF64U, ValType::I32)?,
            Operator::I64TruncF32S => self.call_rt_unop_trapping(Rt::I64TruncF32S, ValType::I64)?,
            Operator::I64TruncF32U => self.call_rt_unop_trapping(Rt::I64TruncF32U, ValType::I64)?,
            Operator::I64TruncF64S => self.call_rt_unop_trapping(Rt::I64TruncF64S, ValType::I64)?,
            Operator::I64TruncF64U => self.call_rt_unop_trapping(Rt::I64TruncF64U, ValType::I64)?,
            Operator::I32TruncSatF32S | Operator::I32TruncSatF64S => {
                self.cast_as(ValType::I32, "i32")?
            }
            Operator::I64TruncSatF32S | Operator::I64TruncSatF64S => {
                self.cast_as(ValType::I64, "i64")?
            }
            Operator::I32TruncSatF32U | Operator::I32TruncSatF64U => {
                self.cast_through(ValType::I32, "u32", "i32")?
            }
            Operator::I64TruncSatF32U | Operator::I64TruncSatF64U => {
                self.cast_through(ValType::I64, "u64", "i64")?
            }
            Operator::GlobalGet { global_index } => self.global_get(global_index)?,
            Operator::GlobalSet { global_index } => self.global_set(global_index)?,
            Operator::I32Load { memarg } => self.load(Helper::LoadI32, ValType::I32, memarg)?,
            Operator::I32Load8U { memarg } => self.load(Helper::Load8U, ValType::I32, memarg)?,
            Operator::I32Load8S { memarg } => self.load(Helper::Load8S, ValType::I32, memarg)?,
            Operator::I32Load16U { memarg } => self.load(Helper::Load16U, ValType::I32, memarg)?,
            Operator::I32Load16S { memarg } => self.load(Helper::Load16S, ValType::I32, memarg)?,
            Operator::I64Load { memarg } => self.load(Helper::LoadI64, ValType::I64, memarg)?,
            Operator::F32Load { memarg } => self.load(Helper::LoadF32, ValType::F32, memarg)?,
            Operator::F64Load { memarg } => self.load(Helper::LoadF64, ValType::F64, memarg)?,
            Operator::I64Load8U { memarg } => self.load(Helper::Load8UI64, ValType::I64, memarg)?,
            Operator::I64Load8S { memarg } => self.load(Helper::Load8SI64, ValType::I64, memarg)?,
            Operator::I64Load16U { memarg } => {
                self.load(Helper::Load16UI64, ValType::I64, memarg)?
            }
            Operator::I64Load16S { memarg } => {
                self.load(Helper::Load16SI64, ValType::I64, memarg)?
            }
            Operator::I64Load32U { memarg } => {
                self.load(Helper::Load32UI64, ValType::I64, memarg)?
            }
            Operator::I64Load32S { memarg } => {
                self.load(Helper::Load32SI64, ValType::I64, memarg)?
            }
            Operator::I32Store { memarg } => self.store(Helper::StoreI32, memarg)?,
            Operator::I32Store8 { memarg } => self.store(Helper::Store8, memarg)?,
            Operator::I32Store16 { memarg } => self.store(Helper::Store16, memarg)?,
            Operator::I64Store { memarg } => self.store(Helper::StoreI64, memarg)?,
            Operator::F32Store { memarg } => self.store(Helper::StoreF32, memarg)?,
            Operator::F64Store { memarg } => self.store(Helper::StoreF64, memarg)?,
            Operator::I64Store8 { memarg } => self.store(Helper::Store8I64, memarg)?,
            Operator::I64Store16 { memarg } => self.store(Helper::Store16I64, memarg)?,
            Operator::I64Store32 { memarg } => self.store(Helper::Store32I64, memarg)?,
            Operator::MemorySize { .. } => self.memory_size()?,
            Operator::MemoryGrow { .. } => self.memory_grow()?,
            // Bulk-memory ops consume three i32s (dest, src/value, len) and
            // produce nothing (`table.fill`, whose value is a funcref, is with
            // the other table instructions below).
            Operator::MemoryFill { mem } => self.memory_fill(mem)?,
            Operator::MemoryCopy { dst_mem, src_mem } => self.memory_copy(dst_mem, src_mem)?,
            Operator::TableCopy {
                dst_table,
                src_table,
            } => self.table_copy(dst_table, src_table)?,
            // Passive-segment init/drop (bulk-memory proposal). `memory.init`/
            // `table.init` copy from a passive segment; `data.drop`/`elem.drop`
            // release it so a later non-zero init traps.
            Operator::MemoryInit { data_index, mem } => self.memory_init(data_index, mem)?,
            Operator::DataDrop { data_index } => self.data_drop(data_index)?,
            Operator::TableInit { elem_index, table } => self.table_init(elem_index, table)?,
            Operator::ElemDrop { elem_index } => self.elem_drop(elem_index)?,
            // Reference types: a `funcref` is a `u32` function index on the
            // operand stack (`u32::MAX` is null).
            Operator::RefNull { hty } => self.ref_null(hty)?,
            Operator::RefFunc { function_index } => self.ref_func(function_index),
            Operator::RefIsNull => self.ref_is_null()?,
            // Table instructions. A table entry and a `funcref` operand are both
            // `u32` function indices (`u32::MAX` is null).
            Operator::TableGet { table } => self.table_get(table)?,
            Operator::TableSet { table } => self.table_set(table)?,
            Operator::TableSize { table } => self.table_size(table)?,
            Operator::TableGrow { table } => self.table_grow(table)?,
            Operator::TableFill { table } => self.table_fill(table)?,
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

    /// Upper bound on the textual length of a single generated expression.
    /// A "stable" value is never spilled, so a long straight-line chain of
    /// operations folds into one Rust expression on one line; without a cap a
    /// large function collapses into a multi-megabyte line that overflows
    /// rustc's parser. `push_combined` spills any expression exceeding this.
    const MAX_EXPR_LEN: usize = 4096;

    fn push(&mut self, val: Val) {
        self.stack.push(val);
    }

    /// Push an expression built from other operands, first spilling it into a
    /// `let` binding when it grows past `MAX_EXPR_LEN`. This bounds the size of
    /// any single generated line while preserving the computed value (the temp
    /// is itself stable, so it inlines cheaply into the next operation).
    fn push_combined(
        &mut self,
        code: String,
        ty: ValType,
        stable: bool,
    ) -> Result<(), TranspileError> {
        if code.len() > Self::MAX_EXPR_LEN {
            return self.materialize(code, ty);
        }
        self.push(Val { code, ty, stable });
        Ok(())
    }

    fn pop(&mut self) -> Result<Val, TranspileError> {
        self.stack.pop().ok_or(TranspileError::StackUnderflow)
    }

    fn line(&mut self, text: impl Into<String>) {
        self.cur.push(Node::Line(text.into()));
    }

    /// Push a terminating statement (control does not fall through afterwards).
    fn term(&mut self, text: impl Into<String>) {
        self.cur.push(Node::Term(text.into()));
    }

    /// Push a structured control-flow node.
    fn node(&mut self, node: Node) {
        self.cur.push(node);
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
}
