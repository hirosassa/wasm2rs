use std::collections::HashSet;

use wasmparser::{FunctionBody, Operator, ValType};

use super::{
    Frame, FrameKind, Helper, LocalFlags, ModuleCtx, Node, Rt, Val, default_value, index_u32,
    rust_type,
};
use crate::TranspileError;

mod calls;
mod classify;
mod cont;
mod control;
mod gc;
mod memvals;
mod numeric;
mod simd;
mod table;

/// The binary operation of an atomic read-modify-write. Since the instance owns
/// its memory exclusively, an RMW lowers to a plain load / combine / store; each
/// variant supplies the "combine" expression over the old value and the operand
/// (`Xchg` just writes the operand). Narrow accesses combine at the full operand
/// width and rely on the store helper to truncate, matching wasm's wrap-on-store.
#[derive(Clone, Copy)]
pub(super) enum RmwOp {
    Add,
    Sub,
    And,
    Or,
    Xor,
    Xchg,
}

impl RmwOp {
    pub(super) fn combine(self, old: &str, value: &str) -> String {
        match self {
            RmwOp::Add => format!("{old}.wrapping_add({value})"),
            RmwOp::Sub => format!("{old}.wrapping_sub({value})"),
            RmwOp::And => format!("({old} & {value})"),
            RmwOp::Or => format!("({old} | {value})"),
            RmwOp::Xor => format!("({old} ^ {value})"),
            RmwOp::Xchg => value.to_string(),
        }
    }

    /// The op code the shared-memory backend's `atomic_rmw` dispatches on:
    /// 0=add 1=sub 2=and 3=or 4=xor 5=xchg.
    pub(super) fn op_code(self) -> u8 {
        match self {
            RmwOp::Add => 0,
            RmwOp::Sub => 1,
            RmwOp::And => 2,
            RmwOp::Or => 3,
            RmwOp::Xor => 4,
            RmwOp::Xchg => 5,
        }
    }
}

/// The value type and access width of an atomic RMW/cmpxchg, the single choice
/// that fixes its load helper, store helper, result type, and (for a narrow
/// access) the low-bit mask used to compare at the access width. `I32As8` means
/// an i32-typed op over an 8-bit cell, etc.
#[derive(Clone, Copy)]
pub(super) enum AtomicWidth {
    I32,
    I32As8,
    I32As16,
    I64,
    I64As8,
    I64As16,
    I64As32,
}

impl AtomicWidth {
    /// `(load helper, store helper, result type, narrow mask)`. The mask is
    /// `None` for a full-width access (compare directly) and the width's low-bit
    /// mask for a narrow one.
    pub(super) fn parts(self) -> (Helper, Helper, ValType, Option<&'static str>) {
        match self {
            AtomicWidth::I32 => (Helper::LoadI32, Helper::StoreI32, ValType::I32, None),
            AtomicWidth::I32As8 => (Helper::Load8U, Helper::Store8, ValType::I32, Some("0xFF")),
            AtomicWidth::I32As16 => (
                Helper::Load16U,
                Helper::Store16,
                ValType::I32,
                Some("0xFFFF"),
            ),
            AtomicWidth::I64 => (Helper::LoadI64, Helper::StoreI64, ValType::I64, None),
            AtomicWidth::I64As8 => (
                Helper::Load8UI64,
                Helper::Store8I64,
                ValType::I64,
                Some("0xFF"),
            ),
            AtomicWidth::I64As16 => (
                Helper::Load16UI64,
                Helper::Store16I64,
                ValType::I64,
                Some("0xFFFF"),
            ),
            AtomicWidth::I64As32 => (
                Helper::Load32UI64,
                Helper::Store32I64,
                ValType::I64,
                Some("0xFFFF_FFFF"),
            ),
        }
    }

    /// The access width in bytes (1/2/4/8), which the shared-memory backend
    /// uses to read/write and mask exactly the accessed cell.
    pub(super) fn byte_width(self) -> usize {
        match self {
            AtomicWidth::I32As8 | AtomicWidth::I64As8 => 1,
            AtomicWidth::I32As16 | AtomicWidth::I64As16 => 2,
            AtomicWidth::I32 | AtomicWidth::I64As32 => 4,
            AtomicWidth::I64 => 8,
        }
    }
}

/// Which phase of a `try` region the current point is being lowered in. A `try`
/// becomes a `catch_unwind` closure (the body) plus a landing-pad `match` (the
/// handlers); the handlers run *outside* the body's closure, so the phase
/// governs which branches escape the closure and thus have no Rust lowering.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum CatchPhase {
    /// Inside the protected body (the `catch_unwind` closure). Only a branch
    /// strictly out of the `try` escapes.
    Body,
    /// Inside a catch handler (the landing pad). Even a branch to the `try`
    /// itself escapes, since the landing pad is not a breakable region.
    Handler,
}

/// One enclosing `try` region on the barrier stack: its frame index and which
/// [`CatchPhase`] of the `try` the current point sits in.
#[derive(Clone, Copy)]
pub(super) struct TryBarrier {
    /// Frame index of the enclosing `try`.
    frame_idx: usize,
    /// Which phase of the `try` the current point is lowering.
    phase: CatchPhase,
}

/// State threaded through the translation of a single function body.
pub(super) struct FuncGen<'a> {
    local_types: Vec<ValType>,
    /// `l{i}` for every local, formatted once: `local.get` is the most frequent
    /// operator and would otherwise run the formatter on every read.
    local_names: Vec<String>,
    /// The locals this function writes (from the module-wide body scan).
    mutable_locals: &'a LocalFlags,
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
    /// Memory-access helpers this function relies on, each paired with the
    /// linear memory index it acts on (0 for the historic single-memory
    /// helpers, `i` for the `_m{i}` variants).
    used_helpers: HashSet<(Helper, u32)>,
    /// Free-function runtime helpers this function relies on.
    used_rt: HashSet<Rt>,
    /// Lane-wise SIMD free-function helpers this function relies on, by name.
    used_simd: HashSet<&'static str>,
    /// `call_indirect` type indices this function dispatches through; each needs
    /// a `call_ref_t{ti}` method on the instance.
    dispatch_sigs: HashSet<u32>,
    /// Whether this function uses legacy exception handling (`try`/`throw`), so
    /// the module needs the exception type emitted.
    uses_eh: bool,
    /// The enclosing `try` regions, innermost last. A `try` lowers to a
    /// `catch_unwind` closure (the body) plus a landing-pad `match` (the
    /// handlers); a branch or `return` leaving either has no Rust lowering.
    try_barriers: Vec<TryBarrier>,
    /// Whether a `return` escapes some `try` body, so the function declares the
    /// shared return-signal flag (`__returning`) and result holders (`__rv{i}`)
    /// that each enclosing try's dispatch re-issues the function return from.
    uses_ret_escape: bool,
    /// Whether a `return_call*` parks its callee in the `__tc` trampoline slot
    /// (set by the driver for a function with a `_tc` body) rather than calling it.
    pub(super) tc_enabled: bool,
    /// The function's own full index when its `return_call`s to itself loop: the
    /// body is wrapped in a synthetic `loop` frame and a self tail call reassigns
    /// the parameters and branches back to it, with no trampoline.
    pub(super) self_loop: Option<u32>,
    /// The function is emitted as a resumable step function. Its `suspend`/
    /// `switch` handling does not honour dead code, so it may still read a
    /// region's result variable even when no path leaves that region.
    pub(super) cont_step: bool,
}

impl<'a> FuncGen<'a> {
    pub(super) fn new(
        params: &[ValType],
        results: &[ValType],
        body: &FunctionBody<'_>,
        ctx: &'a ModuleCtx<'a>,
        mutable_locals: &'a LocalFlags,
        referenced: &LocalFlags,
    ) -> Result<Self, TranspileError> {
        let mut local_types = params.to_vec();
        for local in body.get_locals_reader()? {
            let (count, ty) = local?;
            for _ in 0..count {
                local_types.push(ty);
            }
        }

        let mut cur = Vec::new();
        // Declared locals default to zero; parameters arrive already bound.
        // Consecutive locals of the same Rust type collapse into one tuple
        // binding (`let (mut l1, l2): (i32, i32) = (0, 0);`) so a function with
        // many locals emits a handful of lines rather than one per local; a
        // single local of a type stays a plain scalar binding. `mut` is applied
        // per-local, only to the ones actually mutated.
        // A local no operator names is never read or written, so its binding
        // would be dead; skipping it cannot change behaviour. Step functions
        // reload every local from the frame separately and never use these.
        let mut declared: Vec<(usize, ValType)> = Vec::new();
        for (i, &ty) in local_types.iter().enumerate().skip(params.len()) {
            if referenced.contains(index_u32(i)?) {
                declared.push((i, ty));
            }
        }
        for run in declared.chunk_by(|a, b| a.1 == b.1) {
            let Some(&(i, ty)) = run.first() else {
                continue;
            };
            let rty = rust_type(ty, ctx.type_kinds)?;
            let default = default_value(ty, ctx.type_kinds);
            if run.len() == 1 {
                let keyword = if mutable_locals.contains(index_u32(i)?) {
                    "let mut"
                } else {
                    "let"
                };
                cur.push(Node::Line(format!("{keyword} l{i}: {rty} = {default};")));
            } else {
                let mut names = Vec::with_capacity(run.len());
                for &(j, _) in run {
                    let mutp = if mutable_locals.contains(index_u32(j)?) {
                        "mut "
                    } else {
                        ""
                    };
                    names.push(format!("{mutp}l{j}"));
                }
                let count = run.len();
                let tys = vec![rty; count].join(", ");
                let defaults = vec![default; count].join(", ");
                cur.push(Node::Line(format!(
                    "let ({}): ({tys}) = ({defaults});",
                    names.join(", ")
                )));
            }
        }

        let local_names = (0..local_types.len()).map(|i| format!("l{i}")).collect();

        Ok(Self {
            local_types,
            local_names,
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
            used_simd: HashSet::new(),
            dispatch_sigs: HashSet::new(),
            uses_eh: false,
            try_barriers: Vec::new(),
            uses_ret_escape: false,
            tc_enabled: false,
            self_loop: None,
            cont_step: false,
        })
    }

    pub(super) fn run(
        &mut self,
        body: &FunctionBody<'_>,
        wrap: bool,
    ) -> Result<(), TranspileError> {
        // A branch to the function's own label is a `return` of its results. Only
        // a function that has one is wrapped in a synthetic block frame (which the
        // body's final `end` then closes), so ordinary functions are unchanged.
        // A self-looping function is wrapped in a `loop` frame instead (the driver
        // never combines the two).
        let wrap = wrap || self.self_loop.is_some();
        if wrap {
            let results = self.results.clone();
            let kind = if self.self_loop.is_some() {
                FrameKind::Loop
            } else {
                FrameKind::Block
            };
            self.push_frame_typed(kind, &[], &results, None)?;
        }
        for op in body.get_operators_reader()? {
            self.emit_op(op?)?;
        }
        if wrap {
            self.end_function()?;
        }
        Ok(())
    }

    fn emit_op(&mut self, op: Operator<'_>) -> Result<(), TranspileError> {
        if !self.reachable {
            return self.skip_dead(&op);
        }

        match op {
            Operator::Nop => {}
            Operator::Unreachable => self.emit_unreachable(),
            Operator::LocalGet { local_index } => self.local_get(local_index)?,
            Operator::LocalSet { local_index } => self.local_store(local_index, false)?,
            Operator::LocalTee { local_index } => self.local_store(local_index, true)?,
            Operator::I32Const { value } => self.const_i32(value),
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
            Operator::I64Const { value } => self.const_i64(value),
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
            // Constants round-trip via their exact bit pattern (so NaN/inf
            // survive); float arithmetic and comparisons then map to native
            // operators (Rust float compare yields false for NaN, as wasm
            // requires), and `min`/`max` are deferred (special NaN semantics).
            Operator::F32Const { value } => self.const_f32(value.bits()),
            Operator::F64Const { value } => self.const_f64(value.bits()),
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
            Operator::F32Ceil | Operator::F64Ceil => self.unop_round("ceil")?,
            Operator::F32Floor | Operator::F64Floor => self.unop_round("floor")?,
            Operator::F32Trunc | Operator::F64Trunc => self.unop_round("trunc")?,
            // wasm `nearest` rounds halves to even, i.e. `round_ties_even`.
            Operator::F32Nearest | Operator::F64Nearest => self.unop_round("round_ties_even")?,
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
            Operator::F32DemoteF64 => self.cast_quieted(ValType::F32, "f32")?,
            Operator::F64PromoteF32 => self.cast_quieted(ValType::F64, "f64")?,
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
            // Threads/atomics proposal (loads, stores, rmw and cmpxchg are in
            // `emit_classified`). Fence and wait/notify. On a single instance a fence is a no-op,
            // `notify` wakes nobody and `wait` traps (would block) or returns 1;
            // on a shared memory they are a real fence and park/wake threads.
            Operator::AtomicFence { .. } => self.atomic_fence()?,
            Operator::MemoryAtomicNotify { memarg } => self.atomic_notify(memarg)?,
            Operator::MemoryAtomicWait32 { memarg } => self.atomic_wait(Helper::LoadI32, memarg)?,
            Operator::MemoryAtomicWait64 { memarg } => self.atomic_wait(Helper::LoadI64, memarg)?,
            Operator::MemorySize { mem } => self.memory_size(mem)?,
            Operator::MemoryGrow { mem } => self.memory_grow(mem)?,
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
            Operator::RefNull { hty } => self.ref_null_dispatch(hty)?,
            Operator::RefFunc { function_index } => self.ref_func(function_index),
            Operator::RefIsNull => self.ref_is_null()?,
            Operator::ContNew { cont_type_index } => self.cont_new(cont_type_index)?,
            Operator::Resume {
                cont_type_index,
                resume_table,
            } => self.resume(cont_type_index, &resume_table)?,
            Operator::ContBind {
                argument_index,
                result_index,
            } => self.cont_bind(argument_index, result_index)?,
            Operator::ResumeThrow {
                cont_type_index,
                tag_index,
                resume_table,
            } => self.resume_throw(cont_type_index, tag_index, &resume_table)?,
            // Table instructions. A table entry and a `funcref` operand are both
            // `u32` function indices (`u32::MAX` is null).
            Operator::TableGet { table } => self.table_get(table)?,
            Operator::TableSet { table } => self.table_set(table)?,
            Operator::TableSize { table } => self.table_size(table)?,
            Operator::TableGrow { table } => self.table_grow(table)?,
            Operator::TableFill { table } => self.table_fill(table)?,
            Operator::Drop => {
                // A dropped value that can trap (an out-of-bounds load) must still
                // run: wasm evaluates it even though nothing uses the result.
                let val = self.pop()?;
                if val.may_trap {
                    self.line(format!("let _ = {};", val.code));
                }
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
            Operator::ReturnCall { function_index } => self.return_call(function_index)?,
            Operator::ReturnCallIndirect {
                type_index,
                table_index,
            } => self.return_call_indirect(type_index, table_index)?,
            // Typed function references (heap-free slice): the funcref is a `u32`
            // function index on the operand stack, dispatched like `call_indirect`
            // but taken straight off the stack.
            Operator::CallRef { type_index } => self.call_ref(type_index)?,
            Operator::ReturnCallRef { type_index } => self.return_call_ref(type_index)?,
            // `i31` reference ops: the payload rides as a plain `i32`.
            Operator::RefI31 => self.ref_i31()?,
            Operator::I31GetS => self.i31_get_s()?,
            Operator::I31GetU => self.i31_get_u()?,
            // GC heap objects (phase 4b): `struct`/`array` values are managed
            // `GcRef` handles; fields/elements are `GcSlot`s read/written by index.
            Operator::StructNew { struct_type_index } => self.struct_new(struct_type_index)?,
            Operator::StructNewDefault { struct_type_index } => {
                self.struct_new_default(struct_type_index)?
            }
            Operator::StructGet {
                struct_type_index,
                field_index,
            } => self.struct_get(struct_type_index, field_index, None)?,
            Operator::StructGetS {
                struct_type_index,
                field_index,
            } => self.struct_get(struct_type_index, field_index, Some(true))?,
            Operator::StructGetU {
                struct_type_index,
                field_index,
            } => self.struct_get(struct_type_index, field_index, Some(false))?,
            Operator::StructSet {
                struct_type_index,
                field_index,
            } => self.struct_set(struct_type_index, field_index)?,
            Operator::ArrayNew { array_type_index } => self.array_new(array_type_index)?,
            Operator::ArrayNewDefault { array_type_index } => {
                self.array_new_default(array_type_index)?
            }
            Operator::ArrayNewFixed {
                array_type_index,
                array_size,
            } => self.array_new_fixed(array_type_index, array_size)?,
            Operator::ArrayGet { array_type_index } => self.array_get(array_type_index, None)?,
            Operator::ArrayGetS { array_type_index } => {
                self.array_get(array_type_index, Some(true))?
            }
            Operator::ArrayGetU { array_type_index } => {
                self.array_get(array_type_index, Some(false))?
            }
            Operator::ArraySet { array_type_index } => self.array_set(array_type_index)?,
            Operator::ArrayLen => self.array_len()?,
            Operator::ArrayFill { array_type_index } => self.array_fill(array_type_index)?,
            Operator::ArrayCopy {
                array_type_index_dst,
                array_type_index_src: _,
            } => self.array_copy(array_type_index_dst)?,
            // Build/initialise an array from a passive data segment (numeric
            // elements) or element segment (funcrefs). Using any forces the module
            // stateful (see `uses_array_segment_ops`).
            Operator::ArrayNewData {
                array_type_index,
                array_data_index,
            } => self.array_new_data(array_type_index, array_data_index)?,
            Operator::ArrayInitData {
                array_type_index,
                array_data_index,
            } => self.array_init_data(array_type_index, array_data_index)?,
            Operator::ArrayNewElem {
                array_type_index,
                array_elem_index,
            } => self.array_new_elem(array_type_index, array_elem_index)?,
            Operator::ArrayInitElem {
                array_type_index,
                array_elem_index,
            } => self.array_init_elem(array_type_index, array_elem_index)?,
            // Runtime downcasts (phase 4c-1): each object carries its concrete
            // type id, and a target's compile-time descendant set decides the
            // check. The `NonNull` variants target `(ref $t)` (null fails); the
            // `Nullable` variants target `(ref null $t)` (null matches).
            Operator::RefTestNonNull { hty } => self.ref_test(hty, false)?,
            Operator::RefTestNullable { hty } => self.ref_test(hty, true)?,
            Operator::RefCastNonNull { hty } => self.ref_cast(hty, false)?,
            Operator::RefCastNullable { hty } => self.ref_cast(hty, true)?,
            Operator::BrOnCast {
                relative_depth,
                from_ref_type,
                to_ref_type,
            } => self.br_on_cast(relative_depth, from_ref_type, to_ref_type, true)?,
            Operator::BrOnCastFail {
                relative_depth,
                from_ref_type,
                to_ref_type,
            } => self.br_on_cast(relative_depth, from_ref_type, to_ref_type, false)?,
            // Null/equality reference ops (phase 4c-2). `ref.eq` compares two
            // handles by identity; `ref.as_non_null` narrows a nullable ref
            // (trapping on null); `br_on_null`/`br_on_non_null` branch on
            // nullness (only the latter carries the ref, dropping it otherwise).
            // Bridge the `extern` and `any` hierarchies (forces the module
            // stateful; see `uses_extern_box` in `scan_bodies`).
            Operator::ExternConvertAny => self.extern_convert_any()?,
            Operator::AnyConvertExtern => self.any_convert_extern()?,
            Operator::RefEq => self.ref_eq()?,
            Operator::RefAsNonNull => self.ref_as_non_null()?,
            Operator::BrOnNull { relative_depth } => self.br_on_null(relative_depth)?,
            Operator::BrOnNonNull { relative_depth } => self.br_on_non_null(relative_depth)?,
            Operator::Return => self.emit_return()?,
            // Legacy exception handling: a `try` region protects its body and
            // dispatches thrown exceptions to matching `catch`/`catch_all`
            // handlers; `throw` raises one and `rethrow` re-raises a caught one.
            Operator::Try { blockty } => self.open_try(blockty)?,
            Operator::TryTable { try_table } => self.open_try_table(&try_table)?,
            Operator::Catch { tag_index } => self.handle_catch(Some(tag_index))?,
            Operator::CatchAll => self.handle_catch(None)?,
            Operator::Throw { tag_index } => self.emit_throw(tag_index)?,
            Operator::ThrowRef => self.emit_throw_ref()?,
            Operator::Rethrow { relative_depth } => self.emit_rethrow(relative_depth)?,
            Operator::Delegate { relative_depth } => self.handle_delegate(relative_depth)?,
            // SIMD/v128 (round 1: foundation). A v128 is a `u128`; see `simd.rs`.
            Operator::V128Const { value } => self.v128_const(value.bytes()),
            Operator::V128Load { memarg } => self.load(Helper::LoadV128, ValType::V128, memarg)?,
            Operator::V128Store { memarg } => self.store(Helper::StoreV128, memarg)?,
            // Splat: broadcast a scalar into every lane (pure runtime helpers).
            Operator::I8x16Splat => self.call_rt_unop(Rt::SplatI8x16, ValType::V128)?,
            Operator::I16x8Splat => self.call_rt_unop(Rt::SplatI16x8, ValType::V128)?,
            Operator::I32x4Splat => self.call_rt_unop(Rt::SplatI32x4, ValType::V128)?,
            Operator::I64x2Splat => self.call_rt_unop(Rt::SplatI64x2, ValType::V128)?,
            Operator::F32x4Splat => self.call_rt_unop(Rt::SplatF32x4, ValType::V128)?,
            Operator::F64x2Splat => self.call_rt_unop(Rt::SplatF64x2, ValType::V128)?,
            // Extract one lane as a scalar. `_s`/`_u` variants sign- or
            // zero-extend the sub-word lanes into the i32 result.
            Operator::I8x16ExtractLaneS { lane } => self.i8x16_extract_lane_s(lane)?,
            Operator::I8x16ExtractLaneU { lane } => self.i8x16_extract_lane_u(lane)?,
            Operator::I16x8ExtractLaneS { lane } => self.i16x8_extract_lane_s(lane)?,
            Operator::I16x8ExtractLaneU { lane } => self.i16x8_extract_lane_u(lane)?,
            Operator::I32x4ExtractLane { lane } => self.i32x4_extract_lane(lane)?,
            Operator::I64x2ExtractLane { lane } => self.i64x2_extract_lane(lane)?,
            Operator::F32x4ExtractLane { lane } => self.f32x4_extract_lane(lane)?,
            Operator::F64x2ExtractLane { lane } => self.f64x2_extract_lane(lane)?,
            // Replace one lane with a scalar.
            Operator::I8x16ReplaceLane { lane } => self.i8x16_replace_lane(lane)?,
            Operator::I16x8ReplaceLane { lane } => self.i16x8_replace_lane(lane)?,
            Operator::I32x4ReplaceLane { lane } => self.i32x4_replace_lane(lane)?,
            Operator::I64x2ReplaceLane { lane } => self.i64x2_replace_lane(lane)?,
            Operator::F32x4ReplaceLane { lane } => self.f32x4_replace_lane(lane)?,
            Operator::F64x2ReplaceLane { lane } => self.f64x2_replace_lane(lane)?,
            // Whole-register bitwise operations map straight to `u128` operators.
            Operator::V128And => self.binop_infix("&")?,
            Operator::V128Or => self.binop_infix("|")?,
            Operator::V128Xor => self.binop_infix("^")?,
            Operator::V128Not => self.v128_not()?,
            Operator::V128AndNot => self.v128_andnot()?,
            Operator::V128Bitselect => self.v128_bitselect()?,
            Operator::V128AnyTrue => self.v128_any_true()?,
            // Lane-wise integer arithmetic (wrapping), one helper per lane
            // type; see `simd_rt.rs`. There is no `i8x16.mul` in the spec.
            // Float lane arithmetic (round 3). neg/abs are whole-register
            // sign-bit rewrites (see the helpers); the rest are per-lane helpers
            // (see `simd_rt.rs`).
            Operator::F32x4Neg => self.f32x4_neg()?,
            Operator::F64x2Neg => self.f64x2_neg()?,
            Operator::F32x4Abs => self.f32x4_abs()?,
            Operator::F64x2Abs => self.f64x2_abs()?,
            // Byte permutes: dynamic swizzle and constant-index shuffle.
            Operator::I8x16Shuffle { lanes } => self.call_simd_shuffle(lanes)?,

            // Memory-to-lane loads: splat (broadcast), zero-extend, and single
            // lane load/store. splat/zero take just an address like a plain load.
            Operator::V128Load8Splat { memarg } => {
                self.load(Helper::Load8Splat, ValType::V128, memarg)?
            }
            Operator::V128Load16Splat { memarg } => {
                self.load(Helper::Load16Splat, ValType::V128, memarg)?
            }
            Operator::V128Load32Splat { memarg } => {
                self.load(Helper::Load32Splat, ValType::V128, memarg)?
            }
            Operator::V128Load64Splat { memarg } => {
                self.load(Helper::Load64Splat, ValType::V128, memarg)?
            }
            Operator::V128Load32Zero { memarg } => {
                self.load(Helper::Load32Zero, ValType::V128, memarg)?
            }
            Operator::V128Load64Zero { memarg } => {
                self.load(Helper::Load64Zero, ValType::V128, memarg)?
            }
            Operator::V128Load8x8S { memarg } => {
                self.load(Helper::Load8x8S, ValType::V128, memarg)?
            }
            Operator::V128Load8x8U { memarg } => {
                self.load(Helper::Load8x8U, ValType::V128, memarg)?
            }
            Operator::V128Load16x4S { memarg } => {
                self.load(Helper::Load16x4S, ValType::V128, memarg)?
            }
            Operator::V128Load16x4U { memarg } => {
                self.load(Helper::Load16x4U, ValType::V128, memarg)?
            }
            Operator::V128Load32x2S { memarg } => {
                self.load(Helper::Load32x2S, ValType::V128, memarg)?
            }
            Operator::V128Load32x2U { memarg } => {
                self.load(Helper::Load32x2U, ValType::V128, memarg)?
            }
            Operator::V128Load8Lane { memarg, lane } => {
                self.load_lane(Helper::Load8Lane, memarg, lane)?
            }
            Operator::V128Load16Lane { memarg, lane } => {
                self.load_lane(Helper::Load16Lane, memarg, lane)?
            }
            Operator::V128Load32Lane { memarg, lane } => {
                self.load_lane(Helper::Load32Lane, memarg, lane)?
            }
            Operator::V128Load64Lane { memarg, lane } => {
                self.load_lane(Helper::Load64Lane, memarg, lane)?
            }
            Operator::V128Store8Lane { memarg, lane } => {
                self.store_lane(Helper::Store8Lane, memarg, lane)?
            }
            Operator::V128Store16Lane { memarg, lane } => {
                self.store_lane(Helper::Store16Lane, memarg, lane)?
            }
            Operator::V128Store32Lane { memarg, lane } => {
                self.store_lane(Helper::Store32Lane, memarg, lane)?
            }
            Operator::V128Store64Lane { memarg, lane } => {
                self.store_lane(Helper::Store64Lane, memarg, lane)?
            }
            // Relaxed SIMD, deterministic lowering. Each of these picks one
            // spec-permitted behaviour and reuses the matching non-relaxed helper:
            // relaxed_swizzle == swizzle (index >= 16 -> 0), relaxed_trunc ==
            // trunc_sat (Rust's saturating float->int cast), relaxed_min/max ==
            // the plain lane min/max, relaxed_q15mulr == the saturating one, and
            // relaxed_laneselect == bitselect.
            Operator::I8x16RelaxedLaneselect
            | Operator::I16x8RelaxedLaneselect
            | Operator::I32x4RelaxedLaneselect
            | Operator::I64x2RelaxedLaneselect => self.v128_bitselect()?,
            // SIMD lane helpers and atomic accesses: see `classify.rs`.
            other => return self.emit_classified(other),
        }
        Ok(())
    }

    /// The table-like operator families (SIMD lane helpers, atomic accesses),
    /// each a single dispatch via its classifier in [`classify`].
    fn emit_classified(&mut self, op: Operator<'_>) -> Result<(), TranspileError> {
        if let Some((call, name)) = classify::simd_helper(&op) {
            debug_assert!(
                super::simd_rt::is_helper(name),
                "unknown SIMD helper {name}"
            );
            return match call {
                classify::SimdCall::Unop => self.call_simd_unop(name),
                classify::SimdCall::Binop => self.call_simd_binop(name),
                classify::SimdCall::Ternop => self.call_simd_ternop(name),
                classify::SimdCall::Shift => self.call_simd_shift(name),
                classify::SimdCall::Reduce => self.call_simd_reduce(name),
            };
        }
        if let Some((helper, ty, memarg)) = classify::atomic_load_kind(&op) {
            return self.atomic_load(helper, ty, memarg);
        }
        if let Some((helper, memarg)) = classify::atomic_store_kind(&op) {
            return self.atomic_store(helper, memarg);
        }
        if let Some((width, rmw, memarg)) = classify::atomic_rmw_kind(&op) {
            return self.atomic_rmw(width, rmw, memarg);
        }
        if let Some((width, memarg)) = classify::atomic_cmpxchg_kind(&op) {
            return self.atomic_cmpxchg(width, memarg);
        }
        Err(TranspileError::Unsupported(format!("operator {op:?}")))
    }

    /// Skip an operator that appears in unreachable code, tracking region
    /// nesting so the matching `end`/`else` of the live frame is still handled.
    fn skip_dead(&mut self, op: &Operator<'_>) -> Result<(), TranspileError> {
        match op {
            Operator::Block { .. }
            | Operator::Loop { .. }
            | Operator::If { .. }
            | Operator::Try { .. }
            | Operator::TryTable { .. } => {
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
            // A `catch`/`catch_all` on the live `try` frame starts a fresh,
            // reachable handler even though the preceding arm ended dead.
            Operator::Catch { tag_index } if self.dead_nesting == 0 => {
                self.handle_catch(Some(*tag_index))?;
            }
            Operator::CatchAll if self.dead_nesting == 0 => {
                self.handle_catch(None)?;
            }
            // Like `end`, a `delegate` closes the live `try` frame (or a nested
            // dead region) even though the body ended unreachable.
            Operator::Delegate { relative_depth } => {
                if self.dead_nesting > 0 {
                    self.dead_nesting -= 1;
                } else {
                    self.handle_delegate(*relative_depth)?;
                }
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
        operands: &[&Val],
    ) -> Result<(), TranspileError> {
        let stable = operands.iter().all(|v| v.stable);
        let may_trap = operands.iter().any(|v| v.may_trap);
        self.push_expr(code, ty, stable, may_trap)
    }

    /// Like `push_combined`, for a value whose evaluation can trap (and so must
    /// still run if the value is discarded or left unselected).
    fn push_trapping(&mut self, code: String, ty: ValType) -> Result<(), TranspileError> {
        self.push_expr(code, ty, false, true)
    }

    fn push_expr(
        &mut self,
        code: String,
        ty: ValType,
        stable: bool,
        may_trap: bool,
    ) -> Result<(), TranspileError> {
        if code.len() > Self::MAX_EXPR_LEN {
            return self.materialize(code, ty);
        }
        self.push(Val {
            code,
            ty,
            stable,
            may_trap,
        });
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
        self.freeze_survivors(0)
    }

    /// Run the deferred, possibly trapping operands below the top `keep` ones, in
    /// push order, as `let _ = ..;` statements. A branch or `return` that leaves
    /// them behind would otherwise drop the expression, and its trap, unevaluated.
    fn run_discarded_trapping(&mut self, keep: usize) {
        let discarded = self.stack.len().saturating_sub(keep);
        for i in 0..discarded {
            let val = &self.stack[i];
            if !val.stable && val.may_trap {
                let code = val.code.clone();
                self.line(format!("let _ = {code};"));
            }
        }
    }

    /// Freeze only the deferred operands that may trap into stable temporaries,
    /// so a conditional branch that skips them still evaluates them first.
    fn freeze_trapping(&mut self) -> Result<(), TranspileError> {
        for i in 0..self.stack.len() {
            if self.stack[i].stable || !self.stack[i].may_trap {
                continue;
            }
            let ty = self.stack[i].ty;
            let name = self.fresh_temp();
            // The slot is overwritten by the temporary below, so a `Copy`
            // operand's code is moved out; a non-`Copy` `GcRef` may still be
            // read from its place (a local or global) and is cloned instead.
            let old = std::mem::replace(
                &mut self.stack[i],
                Val {
                    code: name.clone(),
                    ty,
                    stable: true,
                    may_trap: false,
                },
            );
            let code = self.move_val_owned(old)?;
            self.line(format!(
                "let {name}: {} = {code};",
                rust_type(ty, self.ctx.type_kinds)?
            ));
        }
        Ok(())
    }

    /// Freeze the non-stable operands that *survive* an upcoming boundary into
    /// `let` temporaries, leaving the top `keep` operands — the ones the
    /// boundary consumes in place, in program order — as inline expressions.
    ///
    /// A deferred non-stable operand only ever *reads* state (a mutable local,
    /// a mutable or imported global, or a memory load; anything side-effecting
    /// is materialised to a stable temp the moment it is produced). A deferred
    /// read may also trap (`Val::may_trap`); `drop` and `select` run such a value
    /// explicitly so the trap is not lost with the discarded operand.
    /// So evaluating a consumed operand at the boundary cannot change a
    /// survivor, and the consumed operands are evaluated in wasm push order
    /// (left to right) exactly where the boundary emits them. That makes
    /// inlining the consumed operands sound while the survivors are still
    /// pinned against the boundary's own side effects.
    fn freeze_survivors(&mut self, keep: usize) -> Result<(), TranspileError> {
        let survivors = self.stack.len().saturating_sub(keep);
        for i in 0..survivors {
            if self.stack[i].stable {
                continue;
            }
            let ty = self.stack[i].ty;
            // The operand may still be read from its place (a local or global), so
            // a non-`Copy` `GcRef` is cloned rather than moved.
            let code = self.move_val(&self.stack[i])?;
            let name = self.fresh_temp();
            self.line(format!(
                "let {name}: {} = {code};",
                rust_type(ty, self.ctx.type_kinds)?
            ));
            self.stack[i] = Val {
                code: name,
                ty,
                stable: true,
                may_trap: false,
            };
        }
        Ok(())
    }
}
