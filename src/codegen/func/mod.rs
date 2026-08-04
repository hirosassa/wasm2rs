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
}

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
    /// Lane-wise SIMD free-function helpers this function relies on, by name.
    used_simd: HashSet<&'static str>,
    /// `call_indirect` type indices this function dispatches through; each needs
    /// a `call_ref_t{ti}` method on the instance.
    dispatch_sigs: HashSet<u32>,
    /// Whether this function uses legacy exception handling (`try`/`throw`), so
    /// the module needs the exception type emitted.
    uses_eh: bool,
    /// The enclosing `try` regions, innermost last, as `(frame index, in a catch
    /// handler)`. A `try` lowers to a `catch_unwind` closure (the body) plus a
    /// landing-pad `match` (the handlers); a branch or `return` leaving either
    /// has no Rust lowering. In the body (`false`) only a branch strictly out of
    /// the try escapes; in a handler (`true`) even a branch to the try itself
    /// does, since the landing pad is not a breakable region.
    try_barriers: Vec<(usize, bool)>,
    /// Whether a `return` escapes some `try` body, so the function declares the
    /// shared return-signal flag (`__returning`) and result holders (`__rv{i}`)
    /// that each enclosing try's dispatch re-issues the function return from.
    uses_ret_escape: bool,
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
        // Consecutive locals of the same Rust type collapse into one tuple
        // binding (`let (mut l1, l2): (i32, i32) = (0, 0);`) so a function with
        // many locals emits a handful of lines rather than one per local; a
        // single local of a type stays a plain scalar binding. `mut` is applied
        // per-local, only to the ones actually mutated.
        let mut i = params.len();
        while i < local_types.len() {
            let ty = local_types[i];
            let mut run_end = i + 1;
            while run_end < local_types.len() && local_types[run_end] == ty {
                run_end += 1;
            }
            let rty = rust_type(ty)?;
            let default = default_value(ty);
            if run_end - i == 1 {
                let keyword = if mutable_locals.contains(&index_u32(i)?) {
                    "let mut"
                } else {
                    "let"
                };
                cur.push(Node::Line(format!("{keyword} l{i}: {rty} = {default};")));
            } else {
                let mut names = Vec::with_capacity(run_end - i);
                for j in i..run_end {
                    let mutp = if mutable_locals.contains(&index_u32(j)?) {
                        "mut "
                    } else {
                        ""
                    };
                    names.push(format!("{mutp}l{j}"));
                }
                let count = run_end - i;
                let tys = vec![rty; count].join(", ");
                let defaults = vec![default; count].join(", ");
                cur.push(Node::Line(format!(
                    "let ({}): ({tys}) = ({defaults});",
                    names.join(", ")
                )));
            }
            i = run_end;
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
            used_simd: HashSet::new(),
            dispatch_sigs: HashSet::new(),
            uses_eh: false,
            try_barriers: Vec::new(),
            uses_ret_escape: false,
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
            // Threads/atomics proposal. The instance owns its memory exclusively
            // (`&mut self`), so an atomic access lowers to the same code as the
            // plain one — atomic loads/stores reuse the ordinary load/store
            // helpers (narrow atomic loads are always zero-extending).
            Operator::I32AtomicLoad { memarg } => {
                self.load(Helper::LoadI32, ValType::I32, memarg)?
            }
            Operator::I32AtomicLoad8U { memarg } => {
                self.load(Helper::Load8U, ValType::I32, memarg)?
            }
            Operator::I32AtomicLoad16U { memarg } => {
                self.load(Helper::Load16U, ValType::I32, memarg)?
            }
            Operator::I64AtomicLoad { memarg } => {
                self.load(Helper::LoadI64, ValType::I64, memarg)?
            }
            Operator::I64AtomicLoad8U { memarg } => {
                self.load(Helper::Load8UI64, ValType::I64, memarg)?
            }
            Operator::I64AtomicLoad16U { memarg } => {
                self.load(Helper::Load16UI64, ValType::I64, memarg)?
            }
            Operator::I64AtomicLoad32U { memarg } => {
                self.load(Helper::Load32UI64, ValType::I64, memarg)?
            }
            Operator::I32AtomicStore { memarg } => self.store(Helper::StoreI32, memarg)?,
            Operator::I32AtomicStore8 { memarg } => self.store(Helper::Store8, memarg)?,
            Operator::I32AtomicStore16 { memarg } => self.store(Helper::Store16, memarg)?,
            Operator::I64AtomicStore { memarg } => self.store(Helper::StoreI64, memarg)?,
            Operator::I64AtomicStore8 { memarg } => self.store(Helper::Store8I64, memarg)?,
            Operator::I64AtomicStore16 { memarg } => self.store(Helper::Store16I64, memarg)?,
            Operator::I64AtomicStore32 { memarg } => self.store(Helper::Store32I64, memarg)?,
            // Atomic read-modify-write. Each pops (addr, operand), combines the
            // old value with the operand, stores the result, and pushes the old
            // value. The `AtomicWidth` fixes the (load, store, type) triple;
            // narrow variants zero-extend on load and truncate on store.
            Operator::I32AtomicRmwAdd { memarg } => {
                self.atomic_rmw(AtomicWidth::I32, RmwOp::Add, memarg)?
            }
            Operator::I32AtomicRmwSub { memarg } => {
                self.atomic_rmw(AtomicWidth::I32, RmwOp::Sub, memarg)?
            }
            Operator::I32AtomicRmwAnd { memarg } => {
                self.atomic_rmw(AtomicWidth::I32, RmwOp::And, memarg)?
            }
            Operator::I32AtomicRmwOr { memarg } => {
                self.atomic_rmw(AtomicWidth::I32, RmwOp::Or, memarg)?
            }
            Operator::I32AtomicRmwXor { memarg } => {
                self.atomic_rmw(AtomicWidth::I32, RmwOp::Xor, memarg)?
            }
            Operator::I32AtomicRmwXchg { memarg } => {
                self.atomic_rmw(AtomicWidth::I32, RmwOp::Xchg, memarg)?
            }
            Operator::I32AtomicRmw8AddU { memarg } => {
                self.atomic_rmw(AtomicWidth::I32As8, RmwOp::Add, memarg)?
            }
            Operator::I32AtomicRmw8SubU { memarg } => {
                self.atomic_rmw(AtomicWidth::I32As8, RmwOp::Sub, memarg)?
            }
            Operator::I32AtomicRmw8AndU { memarg } => {
                self.atomic_rmw(AtomicWidth::I32As8, RmwOp::And, memarg)?
            }
            Operator::I32AtomicRmw8OrU { memarg } => {
                self.atomic_rmw(AtomicWidth::I32As8, RmwOp::Or, memarg)?
            }
            Operator::I32AtomicRmw8XorU { memarg } => {
                self.atomic_rmw(AtomicWidth::I32As8, RmwOp::Xor, memarg)?
            }
            Operator::I32AtomicRmw8XchgU { memarg } => {
                self.atomic_rmw(AtomicWidth::I32As8, RmwOp::Xchg, memarg)?
            }
            Operator::I32AtomicRmw16AddU { memarg } => {
                self.atomic_rmw(AtomicWidth::I32As16, RmwOp::Add, memarg)?
            }
            Operator::I32AtomicRmw16SubU { memarg } => {
                self.atomic_rmw(AtomicWidth::I32As16, RmwOp::Sub, memarg)?
            }
            Operator::I32AtomicRmw16AndU { memarg } => {
                self.atomic_rmw(AtomicWidth::I32As16, RmwOp::And, memarg)?
            }
            Operator::I32AtomicRmw16OrU { memarg } => {
                self.atomic_rmw(AtomicWidth::I32As16, RmwOp::Or, memarg)?
            }
            Operator::I32AtomicRmw16XorU { memarg } => {
                self.atomic_rmw(AtomicWidth::I32As16, RmwOp::Xor, memarg)?
            }
            Operator::I32AtomicRmw16XchgU { memarg } => {
                self.atomic_rmw(AtomicWidth::I32As16, RmwOp::Xchg, memarg)?
            }
            Operator::I64AtomicRmwAdd { memarg } => {
                self.atomic_rmw(AtomicWidth::I64, RmwOp::Add, memarg)?
            }
            Operator::I64AtomicRmwSub { memarg } => {
                self.atomic_rmw(AtomicWidth::I64, RmwOp::Sub, memarg)?
            }
            Operator::I64AtomicRmwAnd { memarg } => {
                self.atomic_rmw(AtomicWidth::I64, RmwOp::And, memarg)?
            }
            Operator::I64AtomicRmwOr { memarg } => {
                self.atomic_rmw(AtomicWidth::I64, RmwOp::Or, memarg)?
            }
            Operator::I64AtomicRmwXor { memarg } => {
                self.atomic_rmw(AtomicWidth::I64, RmwOp::Xor, memarg)?
            }
            Operator::I64AtomicRmwXchg { memarg } => {
                self.atomic_rmw(AtomicWidth::I64, RmwOp::Xchg, memarg)?
            }
            Operator::I64AtomicRmw8AddU { memarg } => {
                self.atomic_rmw(AtomicWidth::I64As8, RmwOp::Add, memarg)?
            }
            Operator::I64AtomicRmw8SubU { memarg } => {
                self.atomic_rmw(AtomicWidth::I64As8, RmwOp::Sub, memarg)?
            }
            Operator::I64AtomicRmw8AndU { memarg } => {
                self.atomic_rmw(AtomicWidth::I64As8, RmwOp::And, memarg)?
            }
            Operator::I64AtomicRmw8OrU { memarg } => {
                self.atomic_rmw(AtomicWidth::I64As8, RmwOp::Or, memarg)?
            }
            Operator::I64AtomicRmw8XorU { memarg } => {
                self.atomic_rmw(AtomicWidth::I64As8, RmwOp::Xor, memarg)?
            }
            Operator::I64AtomicRmw8XchgU { memarg } => {
                self.atomic_rmw(AtomicWidth::I64As8, RmwOp::Xchg, memarg)?
            }
            Operator::I64AtomicRmw16AddU { memarg } => {
                self.atomic_rmw(AtomicWidth::I64As16, RmwOp::Add, memarg)?
            }
            Operator::I64AtomicRmw16SubU { memarg } => {
                self.atomic_rmw(AtomicWidth::I64As16, RmwOp::Sub, memarg)?
            }
            Operator::I64AtomicRmw16AndU { memarg } => {
                self.atomic_rmw(AtomicWidth::I64As16, RmwOp::And, memarg)?
            }
            Operator::I64AtomicRmw16OrU { memarg } => {
                self.atomic_rmw(AtomicWidth::I64As16, RmwOp::Or, memarg)?
            }
            Operator::I64AtomicRmw16XorU { memarg } => {
                self.atomic_rmw(AtomicWidth::I64As16, RmwOp::Xor, memarg)?
            }
            Operator::I64AtomicRmw16XchgU { memarg } => {
                self.atomic_rmw(AtomicWidth::I64As16, RmwOp::Xchg, memarg)?
            }
            Operator::I64AtomicRmw32AddU { memarg } => {
                self.atomic_rmw(AtomicWidth::I64As32, RmwOp::Add, memarg)?
            }
            Operator::I64AtomicRmw32SubU { memarg } => {
                self.atomic_rmw(AtomicWidth::I64As32, RmwOp::Sub, memarg)?
            }
            Operator::I64AtomicRmw32AndU { memarg } => {
                self.atomic_rmw(AtomicWidth::I64As32, RmwOp::And, memarg)?
            }
            Operator::I64AtomicRmw32OrU { memarg } => {
                self.atomic_rmw(AtomicWidth::I64As32, RmwOp::Or, memarg)?
            }
            Operator::I64AtomicRmw32XorU { memarg } => {
                self.atomic_rmw(AtomicWidth::I64As32, RmwOp::Xor, memarg)?
            }
            Operator::I64AtomicRmw32XchgU { memarg } => {
                self.atomic_rmw(AtomicWidth::I64As32, RmwOp::Xchg, memarg)?
            }
            // Atomic compare-exchange. The `AtomicWidth` also carries the narrow
            // mask, so a narrow variant compares the operand at the access width.
            Operator::I32AtomicRmwCmpxchg { memarg } => {
                self.atomic_cmpxchg(AtomicWidth::I32, memarg)?
            }
            Operator::I32AtomicRmw8CmpxchgU { memarg } => {
                self.atomic_cmpxchg(AtomicWidth::I32As8, memarg)?
            }
            Operator::I32AtomicRmw16CmpxchgU { memarg } => {
                self.atomic_cmpxchg(AtomicWidth::I32As16, memarg)?
            }
            Operator::I64AtomicRmwCmpxchg { memarg } => {
                self.atomic_cmpxchg(AtomicWidth::I64, memarg)?
            }
            Operator::I64AtomicRmw8CmpxchgU { memarg } => {
                self.atomic_cmpxchg(AtomicWidth::I64As8, memarg)?
            }
            Operator::I64AtomicRmw16CmpxchgU { memarg } => {
                self.atomic_cmpxchg(AtomicWidth::I64As16, memarg)?
            }
            Operator::I64AtomicRmw32CmpxchgU { memarg } => {
                self.atomic_cmpxchg(AtomicWidth::I64As32, memarg)?
            }
            // Fence and wait/notify. A fence is a no-op on a single instance;
            // `notify` wakes nobody; `wait` traps (would block) or returns 1.
            Operator::AtomicFence => {}
            Operator::MemoryAtomicNotify { .. } => self.atomic_notify()?,
            Operator::MemoryAtomicWait32 { memarg } => self.atomic_wait(Helper::LoadI32, memarg)?,
            Operator::MemoryAtomicWait64 { memarg } => self.atomic_wait(Helper::LoadI64, memarg)?,
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
            // Legacy exception handling: a `try` region protects its body and
            // dispatches thrown exceptions to matching `catch`/`catch_all`
            // handlers; `throw` raises one and `rethrow` re-raises a caught one.
            Operator::Try { blockty } => self.open_try(blockty)?,
            Operator::Catch { tag_index } => self.handle_catch(Some(tag_index))?,
            Operator::CatchAll => self.handle_catch(None)?,
            Operator::Throw { tag_index } => self.emit_throw(tag_index)?,
            Operator::Rethrow { relative_depth } => self.emit_rethrow(relative_depth)?,
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
            Operator::I8x16ExtractLaneS { lane } => {
                self.extract_lane(lane, 8, ValType::I32, |s| {
                    format!("({s} as u8 as i8 as i32)")
                })?
            }
            Operator::I8x16ExtractLaneU { lane } => {
                self.extract_lane(lane, 8, ValType::I32, |s| format!("({s} as u8 as i32)"))?
            }
            Operator::I16x8ExtractLaneS { lane } => {
                self.extract_lane(lane, 16, ValType::I32, |s| {
                    format!("({s} as u16 as i16 as i32)")
                })?
            }
            Operator::I16x8ExtractLaneU { lane } => {
                self.extract_lane(lane, 16, ValType::I32, |s| format!("({s} as u16 as i32)"))?
            }
            Operator::I32x4ExtractLane { lane } => {
                self.extract_lane(lane, 32, ValType::I32, |s| format!("({s} as u32 as i32)"))?
            }
            Operator::I64x2ExtractLane { lane } => {
                self.extract_lane(lane, 64, ValType::I64, |s| format!("({s} as u64 as i64)"))?
            }
            Operator::F32x4ExtractLane { lane } => {
                self.extract_lane(lane, 32, ValType::F32, |s| {
                    format!("f32::from_bits({s} as u32)")
                })?
            }
            Operator::F64x2ExtractLane { lane } => {
                self.extract_lane(lane, 64, ValType::F64, |s| {
                    format!("f64::from_bits({s} as u64)")
                })?
            }
            // Replace one lane with a scalar.
            Operator::I8x16ReplaceLane { lane } => {
                self.replace_lane(lane, 8, "0xFFu128", |x| format!("{x} as u8 as u128"))?
            }
            Operator::I16x8ReplaceLane { lane } => {
                self.replace_lane(lane, 16, "0xFFFFu128", |x| format!("{x} as u16 as u128"))?
            }
            Operator::I32x4ReplaceLane { lane } => {
                self.replace_lane(lane, 32, "0xFFFFFFFFu128", |x| {
                    format!("{x} as u32 as u128")
                })?
            }
            Operator::I64x2ReplaceLane { lane } => {
                self.replace_lane(lane, 64, "0xFFFFFFFFFFFFFFFFu128", |x| {
                    format!("{x} as u64 as u128")
                })?
            }
            Operator::F32x4ReplaceLane { lane } => {
                self.replace_lane(lane, 32, "0xFFFFFFFFu128", |x| {
                    format!("{x}.to_bits() as u128")
                })?
            }
            Operator::F64x2ReplaceLane { lane } => {
                self.replace_lane(lane, 64, "0xFFFFFFFFFFFFFFFFu128", |x| {
                    format!("{x}.to_bits() as u128")
                })?
            }
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
            Operator::I8x16Add => self.call_simd_binop("i8x16_add")?,
            Operator::I16x8Add => self.call_simd_binop("i16x8_add")?,
            Operator::I32x4Add => self.call_simd_binop("i32x4_add")?,
            Operator::I64x2Add => self.call_simd_binop("i64x2_add")?,
            Operator::I8x16Sub => self.call_simd_binop("i8x16_sub")?,
            Operator::I16x8Sub => self.call_simd_binop("i16x8_sub")?,
            Operator::I32x4Sub => self.call_simd_binop("i32x4_sub")?,
            Operator::I64x2Sub => self.call_simd_binop("i64x2_sub")?,
            Operator::I16x8Mul => self.call_simd_binop("i16x8_mul")?,
            Operator::I32x4Mul => self.call_simd_binop("i32x4_mul")?,
            Operator::I64x2Mul => self.call_simd_binop("i64x2_mul")?,
            Operator::I8x16Neg => self.call_simd_unop("i8x16_neg")?,
            Operator::I16x8Neg => self.call_simd_unop("i16x8_neg")?,
            Operator::I32x4Neg => self.call_simd_unop("i32x4_neg")?,
            Operator::I64x2Neg => self.call_simd_unop("i64x2_neg")?,
            // Float lane arithmetic (round 3). neg/abs are exact sign-bit
            // rewrites on the whole register; the rest are per-lane helpers
            // (see `simd_rt.rs`). The sign masks tile one lane's sign / magnitude
            // bit pattern across the 128-bit register.
            Operator::F32x4Neg => {
                self.v128_mask_op('^', 0x8000_0000_8000_0000_8000_0000_8000_0000)?
            }
            Operator::F64x2Neg => {
                self.v128_mask_op('^', 0x8000_0000_0000_0000_8000_0000_0000_0000)?
            }
            Operator::F32x4Abs => {
                self.v128_mask_op('&', 0x7fff_ffff_7fff_ffff_7fff_ffff_7fff_ffff)?
            }
            Operator::F64x2Abs => {
                self.v128_mask_op('&', 0x7fff_ffff_ffff_ffff_7fff_ffff_ffff_ffff)?
            }
            Operator::F32x4Add => self.call_simd_binop("f32x4_add")?,
            Operator::F64x2Add => self.call_simd_binop("f64x2_add")?,
            Operator::F32x4Sub => self.call_simd_binop("f32x4_sub")?,
            Operator::F64x2Sub => self.call_simd_binop("f64x2_sub")?,
            Operator::F32x4Mul => self.call_simd_binop("f32x4_mul")?,
            Operator::F64x2Mul => self.call_simd_binop("f64x2_mul")?,
            Operator::F32x4Div => self.call_simd_binop("f32x4_div")?,
            Operator::F64x2Div => self.call_simd_binop("f64x2_div")?,
            Operator::F32x4Min => self.call_simd_binop("f32x4_min")?,
            Operator::F64x2Min => self.call_simd_binop("f64x2_min")?,
            Operator::F32x4Max => self.call_simd_binop("f32x4_max")?,
            Operator::F64x2Max => self.call_simd_binop("f64x2_max")?,
            Operator::F32x4PMin => self.call_simd_binop("f32x4_pmin")?,
            Operator::F64x2PMin => self.call_simd_binop("f64x2_pmin")?,
            Operator::F32x4PMax => self.call_simd_binop("f32x4_pmax")?,
            Operator::F64x2PMax => self.call_simd_binop("f64x2_pmax")?,
            Operator::F32x4Sqrt => self.call_simd_unop("f32x4_sqrt")?,
            Operator::F64x2Sqrt => self.call_simd_unop("f64x2_sqrt")?,
            Operator::F32x4Ceil => self.call_simd_unop("f32x4_ceil")?,
            Operator::F64x2Ceil => self.call_simd_unop("f64x2_ceil")?,
            Operator::F32x4Floor => self.call_simd_unop("f32x4_floor")?,
            Operator::F64x2Floor => self.call_simd_unop("f64x2_floor")?,
            Operator::F32x4Trunc => self.call_simd_unop("f32x4_trunc")?,
            Operator::F64x2Trunc => self.call_simd_unop("f64x2_trunc")?,
            Operator::F32x4Nearest => self.call_simd_unop("f32x4_nearest")?,
            Operator::F64x2Nearest => self.call_simd_unop("f64x2_nearest")?,
            // Lane comparisons (round 4): each yields an all-ones/zero lane mask
            // (see `simd_rt.rs`). i64x2 has only signed integer forms.
            Operator::I8x16Eq => self.call_simd_binop("i8x16_eq")?,
            Operator::I8x16Ne => self.call_simd_binop("i8x16_ne")?,
            Operator::I8x16LtS => self.call_simd_binop("i8x16_lt_s")?,
            Operator::I8x16LtU => self.call_simd_binop("i8x16_lt_u")?,
            Operator::I8x16GtS => self.call_simd_binop("i8x16_gt_s")?,
            Operator::I8x16GtU => self.call_simd_binop("i8x16_gt_u")?,
            Operator::I8x16LeS => self.call_simd_binop("i8x16_le_s")?,
            Operator::I8x16LeU => self.call_simd_binop("i8x16_le_u")?,
            Operator::I8x16GeS => self.call_simd_binop("i8x16_ge_s")?,
            Operator::I8x16GeU => self.call_simd_binop("i8x16_ge_u")?,
            Operator::I16x8Eq => self.call_simd_binop("i16x8_eq")?,
            Operator::I16x8Ne => self.call_simd_binop("i16x8_ne")?,
            Operator::I16x8LtS => self.call_simd_binop("i16x8_lt_s")?,
            Operator::I16x8LtU => self.call_simd_binop("i16x8_lt_u")?,
            Operator::I16x8GtS => self.call_simd_binop("i16x8_gt_s")?,
            Operator::I16x8GtU => self.call_simd_binop("i16x8_gt_u")?,
            Operator::I16x8LeS => self.call_simd_binop("i16x8_le_s")?,
            Operator::I16x8LeU => self.call_simd_binop("i16x8_le_u")?,
            Operator::I16x8GeS => self.call_simd_binop("i16x8_ge_s")?,
            Operator::I16x8GeU => self.call_simd_binop("i16x8_ge_u")?,
            Operator::I32x4Eq => self.call_simd_binop("i32x4_eq")?,
            Operator::I32x4Ne => self.call_simd_binop("i32x4_ne")?,
            Operator::I32x4LtS => self.call_simd_binop("i32x4_lt_s")?,
            Operator::I32x4LtU => self.call_simd_binop("i32x4_lt_u")?,
            Operator::I32x4GtS => self.call_simd_binop("i32x4_gt_s")?,
            Operator::I32x4GtU => self.call_simd_binop("i32x4_gt_u")?,
            Operator::I32x4LeS => self.call_simd_binop("i32x4_le_s")?,
            Operator::I32x4LeU => self.call_simd_binop("i32x4_le_u")?,
            Operator::I32x4GeS => self.call_simd_binop("i32x4_ge_s")?,
            Operator::I32x4GeU => self.call_simd_binop("i32x4_ge_u")?,
            Operator::I64x2Eq => self.call_simd_binop("i64x2_eq")?,
            Operator::I64x2Ne => self.call_simd_binop("i64x2_ne")?,
            Operator::I64x2LtS => self.call_simd_binop("i64x2_lt_s")?,
            Operator::I64x2GtS => self.call_simd_binop("i64x2_gt_s")?,
            Operator::I64x2LeS => self.call_simd_binop("i64x2_le_s")?,
            Operator::I64x2GeS => self.call_simd_binop("i64x2_ge_s")?,
            Operator::F32x4Eq => self.call_simd_binop("f32x4_eq")?,
            Operator::F32x4Ne => self.call_simd_binop("f32x4_ne")?,
            Operator::F32x4Lt => self.call_simd_binop("f32x4_lt")?,
            Operator::F32x4Gt => self.call_simd_binop("f32x4_gt")?,
            Operator::F32x4Le => self.call_simd_binop("f32x4_le")?,
            Operator::F32x4Ge => self.call_simd_binop("f32x4_ge")?,
            Operator::F64x2Eq => self.call_simd_binop("f64x2_eq")?,
            Operator::F64x2Ne => self.call_simd_binop("f64x2_ne")?,
            Operator::F64x2Lt => self.call_simd_binop("f64x2_lt")?,
            Operator::F64x2Gt => self.call_simd_binop("f64x2_gt")?,
            Operator::F64x2Le => self.call_simd_binop("f64x2_le")?,
            Operator::F64x2Ge => self.call_simd_binop("f64x2_ge")?,
            // Lane shifts (round 4): shift each lane by an i32 count mod width.
            Operator::I8x16Shl => self.call_simd_shift("i8x16_shl")?,
            Operator::I16x8Shl => self.call_simd_shift("i16x8_shl")?,
            Operator::I32x4Shl => self.call_simd_shift("i32x4_shl")?,
            Operator::I64x2Shl => self.call_simd_shift("i64x2_shl")?,
            Operator::I8x16ShrS => self.call_simd_shift("i8x16_shr_s")?,
            Operator::I16x8ShrS => self.call_simd_shift("i16x8_shr_s")?,
            Operator::I32x4ShrS => self.call_simd_shift("i32x4_shr_s")?,
            Operator::I64x2ShrS => self.call_simd_shift("i64x2_shr_s")?,
            Operator::I8x16ShrU => self.call_simd_shift("i8x16_shr_u")?,
            Operator::I16x8ShrU => self.call_simd_shift("i16x8_shr_u")?,
            Operator::I32x4ShrU => self.call_simd_shift("i32x4_shr_u")?,
            Operator::I64x2ShrU => self.call_simd_shift("i64x2_shr_u")?,
            // Saturating add/sub (clamp instead of wrap).
            Operator::I8x16AddSatS => self.call_simd_binop("i8x16_add_sat_s")?,
            Operator::I8x16AddSatU => self.call_simd_binop("i8x16_add_sat_u")?,
            Operator::I16x8AddSatS => self.call_simd_binop("i16x8_add_sat_s")?,
            Operator::I16x8AddSatU => self.call_simd_binop("i16x8_add_sat_u")?,
            Operator::I8x16SubSatS => self.call_simd_binop("i8x16_sub_sat_s")?,
            Operator::I8x16SubSatU => self.call_simd_binop("i8x16_sub_sat_u")?,
            Operator::I16x8SubSatS => self.call_simd_binop("i16x8_sub_sat_s")?,
            Operator::I16x8SubSatU => self.call_simd_binop("i16x8_sub_sat_u")?,
            // Widen the low/high half of the lanes to double width.
            Operator::I16x8ExtendLowI8x16S => self.call_simd_unop("i16x8_extend_low_i8x16_s")?,
            Operator::I16x8ExtendHighI8x16S => self.call_simd_unop("i16x8_extend_high_i8x16_s")?,
            Operator::I16x8ExtendLowI8x16U => self.call_simd_unop("i16x8_extend_low_i8x16_u")?,
            Operator::I16x8ExtendHighI8x16U => self.call_simd_unop("i16x8_extend_high_i8x16_u")?,
            Operator::I32x4ExtendLowI16x8S => self.call_simd_unop("i32x4_extend_low_i16x8_s")?,
            Operator::I32x4ExtendHighI16x8S => self.call_simd_unop("i32x4_extend_high_i16x8_s")?,
            Operator::I32x4ExtendLowI16x8U => self.call_simd_unop("i32x4_extend_low_i16x8_u")?,
            Operator::I32x4ExtendHighI16x8U => self.call_simd_unop("i32x4_extend_high_i16x8_u")?,
            Operator::I64x2ExtendLowI32x4S => self.call_simd_unop("i64x2_extend_low_i32x4_s")?,
            Operator::I64x2ExtendHighI32x4S => self.call_simd_unop("i64x2_extend_high_i32x4_s")?,
            Operator::I64x2ExtendLowI32x4U => self.call_simd_unop("i64x2_extend_low_i32x4_u")?,
            Operator::I64x2ExtendHighI32x4U => self.call_simd_unop("i64x2_extend_high_i32x4_u")?,
            // Saturate two vectors' lanes to half width and concatenate.
            Operator::I8x16NarrowI16x8S => self.call_simd_binop("i8x16_narrow_i16x8_s")?,
            Operator::I8x16NarrowI16x8U => self.call_simd_binop("i8x16_narrow_i16x8_u")?,
            Operator::I16x8NarrowI32x4S => self.call_simd_binop("i16x8_narrow_i32x4_s")?,
            Operator::I16x8NarrowI32x4U => self.call_simd_binop("i16x8_narrow_i32x4_u")?,
            // Widening pairwise multiply of the low/high half of both vectors.
            Operator::I16x8ExtMulLowI8x16S => self.call_simd_binop("i16x8_extmul_low_i8x16_s")?,
            Operator::I16x8ExtMulHighI8x16S => self.call_simd_binop("i16x8_extmul_high_i8x16_s")?,
            Operator::I16x8ExtMulLowI8x16U => self.call_simd_binop("i16x8_extmul_low_i8x16_u")?,
            Operator::I16x8ExtMulHighI8x16U => self.call_simd_binop("i16x8_extmul_high_i8x16_u")?,
            Operator::I32x4ExtMulLowI16x8S => self.call_simd_binop("i32x4_extmul_low_i16x8_s")?,
            Operator::I32x4ExtMulHighI16x8S => self.call_simd_binop("i32x4_extmul_high_i16x8_s")?,
            Operator::I32x4ExtMulLowI16x8U => self.call_simd_binop("i32x4_extmul_low_i16x8_u")?,
            Operator::I32x4ExtMulHighI16x8U => self.call_simd_binop("i32x4_extmul_high_i16x8_u")?,
            Operator::I64x2ExtMulLowI32x4S => self.call_simd_binop("i64x2_extmul_low_i32x4_s")?,
            Operator::I64x2ExtMulHighI32x4S => self.call_simd_binop("i64x2_extmul_high_i32x4_s")?,
            Operator::I64x2ExtMulLowI32x4U => self.call_simd_binop("i64x2_extmul_low_i32x4_u")?,
            Operator::I64x2ExtMulHighI32x4U => self.call_simd_binop("i64x2_extmul_high_i32x4_u")?,
            // Widening add of each adjacent lane pair of one vector.
            Operator::I16x8ExtAddPairwiseI8x16S => {
                self.call_simd_unop("i16x8_extadd_pairwise_i8x16_s")?
            }
            Operator::I16x8ExtAddPairwiseI8x16U => {
                self.call_simd_unop("i16x8_extadd_pairwise_i8x16_u")?
            }
            Operator::I32x4ExtAddPairwiseI16x8S => {
                self.call_simd_unop("i32x4_extadd_pairwise_i16x8_s")?
            }
            Operator::I32x4ExtAddPairwiseI16x8U => {
                self.call_simd_unop("i32x4_extadd_pairwise_i16x8_u")?
            }
            // Widening dot product (i16 pairs to i32) and Q15 rounding multiply.
            Operator::I32x4DotI16x8S => self.call_simd_binop("i32x4_dot_i16x8_s")?,
            Operator::I16x8Q15MulrSatS => self.call_simd_binop("i16x8_q15mulr_sat_s")?,

            // Float <-> integer lane conversions (trunc_sat, convert, demote,
            // promote). Each is a single-vector width-changing helper.
            Operator::I32x4TruncSatF32x4S => self.call_simd_unop("i32x4_trunc_sat_f32x4_s")?,
            Operator::I32x4TruncSatF32x4U => self.call_simd_unop("i32x4_trunc_sat_f32x4_u")?,
            Operator::I32x4TruncSatF64x2SZero => {
                self.call_simd_unop("i32x4_trunc_sat_f64x2_s_zero")?
            }
            Operator::I32x4TruncSatF64x2UZero => {
                self.call_simd_unop("i32x4_trunc_sat_f64x2_u_zero")?
            }
            Operator::F32x4ConvertI32x4S => self.call_simd_unop("f32x4_convert_i32x4_s")?,
            Operator::F32x4ConvertI32x4U => self.call_simd_unop("f32x4_convert_i32x4_u")?,
            Operator::F64x2ConvertLowI32x4S => self.call_simd_unop("f64x2_convert_low_i32x4_s")?,
            Operator::F64x2ConvertLowI32x4U => self.call_simd_unop("f64x2_convert_low_i32x4_u")?,
            Operator::F32x4DemoteF64x2Zero => self.call_simd_unop("f32x4_demote_f64x2_zero")?,
            Operator::F64x2PromoteLowF32x4 => self.call_simd_unop("f64x2_promote_low_f32x4")?,

            // Integer lane abs, unsigned rounding average, and popcount.
            Operator::I8x16Abs => self.call_simd_unop("i8x16_abs")?,
            Operator::I16x8Abs => self.call_simd_unop("i16x8_abs")?,
            Operator::I32x4Abs => self.call_simd_unop("i32x4_abs")?,
            Operator::I64x2Abs => self.call_simd_unop("i64x2_abs")?,
            Operator::I8x16AvgrU => self.call_simd_binop("i8x16_avgr_u")?,
            Operator::I16x8AvgrU => self.call_simd_binop("i16x8_avgr_u")?,
            Operator::I8x16Popcnt => self.call_simd_unop("i8x16_popcnt")?,

            // Lane-reducing predicates: all_true and bitmask (v128 -> i32).
            Operator::I8x16AllTrue => self.call_simd_reduce("i8x16_all_true")?,
            Operator::I16x8AllTrue => self.call_simd_reduce("i16x8_all_true")?,
            Operator::I32x4AllTrue => self.call_simd_reduce("i32x4_all_true")?,
            Operator::I64x2AllTrue => self.call_simd_reduce("i64x2_all_true")?,
            Operator::I8x16Bitmask => self.call_simd_reduce("i8x16_bitmask")?,
            Operator::I16x8Bitmask => self.call_simd_reduce("i16x8_bitmask")?,
            Operator::I32x4Bitmask => self.call_simd_reduce("i32x4_bitmask")?,
            Operator::I64x2Bitmask => self.call_simd_reduce("i64x2_bitmask")?,

            // Byte permutes: dynamic swizzle and constant-index shuffle.
            Operator::I8x16Swizzle => self.call_simd_binop("i8x16_swizzle")?,
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
            Operator::I8x16RelaxedSwizzle => self.call_simd_binop("i8x16_swizzle")?,
            Operator::I32x4RelaxedTruncF32x4S => self.call_simd_unop("i32x4_trunc_sat_f32x4_s")?,
            Operator::I32x4RelaxedTruncF32x4U => self.call_simd_unop("i32x4_trunc_sat_f32x4_u")?,
            Operator::I32x4RelaxedTruncF64x2SZero => {
                self.call_simd_unop("i32x4_trunc_sat_f64x2_s_zero")?
            }
            Operator::I32x4RelaxedTruncF64x2UZero => {
                self.call_simd_unop("i32x4_trunc_sat_f64x2_u_zero")?
            }
            Operator::F32x4RelaxedMin => self.call_simd_binop("f32x4_min")?,
            Operator::F32x4RelaxedMax => self.call_simd_binop("f32x4_max")?,
            Operator::F64x2RelaxedMin => self.call_simd_binop("f64x2_min")?,
            Operator::F64x2RelaxedMax => self.call_simd_binop("f64x2_max")?,
            Operator::I16x8RelaxedQ15mulrS => self.call_simd_binop("i16x8_q15mulr_sat_s")?,
            Operator::I8x16RelaxedLaneselect
            | Operator::I16x8RelaxedLaneselect
            | Operator::I32x4RelaxedLaneselect
            | Operator::I64x2RelaxedLaneselect => self.v128_bitselect()?,
            // Relaxed fused-multiply-add and dot-product-accumulate: ternary lane
            // ops (relaxed_dot below is the one binary case). The deterministic
            // lowering is unfused a*b(+/-)c and a saturating integer dot.
            Operator::F32x4RelaxedMadd => self.call_simd_ternop("f32x4_relaxed_madd")?,
            Operator::F32x4RelaxedNmadd => self.call_simd_ternop("f32x4_relaxed_nmadd")?,
            Operator::F64x2RelaxedMadd => self.call_simd_ternop("f64x2_relaxed_madd")?,
            Operator::F64x2RelaxedNmadd => self.call_simd_ternop("f64x2_relaxed_nmadd")?,
            Operator::I16x8RelaxedDotI8x16I7x16S => {
                self.call_simd_binop("i16x8_relaxed_dot_i8x16_i7x16_s")?
            }
            Operator::I32x4RelaxedDotI8x16I7x16AddS => {
                self.call_simd_ternop("i32x4_relaxed_dot_i8x16_i7x16_add_s")?
            }
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
            Operator::Block { .. }
            | Operator::Loop { .. }
            | Operator::If { .. }
            | Operator::Try { .. } => {
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
        self.freeze_survivors(0)
    }

    /// Freeze the non-stable operands that *survive* an upcoming boundary into
    /// `let` temporaries, leaving the top `keep` operands — the ones the
    /// boundary consumes in place, in program order — as inline expressions.
    ///
    /// A deferred non-stable operand only ever *reads* state (a mutable local,
    /// a mutable or imported global, or a memory load; anything side-effecting
    /// or trapping is materialised to a stable temp the moment it is produced).
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
