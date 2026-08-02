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

/// An imported function: its signature. Imported functions occupy the low end
/// of the function index space and are dispatched through the injected host.
pub(crate) struct ImportInfo {
    pub params: Vec<ValType>,
    pub results: Vec<ValType>,
}

/// An imported global: its type and mutability. Imported globals occupy the low
/// end of the global index space and are read/written through the injected host
/// (`get_global{k}`/`set_global{k}`), preserving host sharing.
pub(crate) struct ImportedGlobalInfo {
    pub ty: ValType,
    pub mutable: bool,
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
    LoadI64,
    LoadF32,
    LoadF64,
    Load8UI64,
    Load8SI64,
    Load16UI64,
    Load16SI64,
    Load32UI64,
    Load32SI64,
    StoreI32,
    Store8,
    Store16,
    StoreI64,
    StoreF32,
    StoreF64,
    Store8I64,
    Store16I64,
    Store32I64,
    Grow,
    MemoryFill,
    MemoryCopy,
    TableCopy,
}

/// A free-standing runtime helper function emitted at module scope on demand.
/// Unlike [`Helper`], these do not touch instance state (memory/globals), so
/// they are plain `fn`s usable from both stateless and stateful modules — used
/// for operations whose wasm semantics differ from Rust's built-in operators.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Rt {
    F32Min,
    F32Max,
    F64Min,
    F64Max,
    I32TruncF32S,
    I32TruncF32U,
    I32TruncF64S,
    I32TruncF64U,
    I64TruncF32S,
    I64TruncF32U,
    I64TruncF64S,
    I64TruncF64U,
}

/// The rendered source of one function plus the helpers it relies on.
struct GenFn {
    src: String,
    /// Instance-method memory helpers.
    helpers: HashSet<Helper>,
    /// Module-scope free-function runtime helpers.
    rt: HashSet<Rt>,
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
    /// Imported functions, occupying function indices `0..imports.len()`.
    imports: &'a [ImportInfo],
    /// Locally-defined functions, occupying the indices after the imports.
    funcs: &'a [FuncInput<'a>],
    /// Every function type, so `call_indirect` can resolve its declared type
    /// index back to a signature.
    types: &'a [TypeSig],
    /// Per-imported-global `(type, mutable)`, occupying the low global indices.
    imported_globals: Vec<(ValType, bool)>,
    /// Per-defined-global `(type, mutable)`, indexed after the imported globals.
    globals: Vec<(ValType, bool)>,
    /// Whether the module declares linear memory (so `self.memory` exists).
    has_memory: bool,
    /// Whether the module declares a table (so `self.table` exists).
    has_table: bool,
    /// Whether functions are emitted as `&mut self` methods (stateful module).
    is_method: bool,
}

impl ModuleCtx<'_> {
    /// The number of functions in the shared index space (imports then defined),
    /// i.e. the range of valid full indices.
    fn func_count(&self) -> usize {
        self.imports.len() + self.funcs.len()
    }

    /// The signature `(params, results)` of the function at full index `fidx`,
    /// spanning imports then defined functions.
    fn full_sig(&self, fidx: usize) -> Option<(&[ValType], &[ValType])> {
        let n_imports = self.imports.len();
        if fidx < n_imports {
            let im = &self.imports[fidx];
            Some((&im.params, &im.results))
        } else {
            let f = self.funcs.get(fidx - n_imports)?;
            Some((f.params, f.results))
        }
    }

    /// The Rust call expression for invoking the function at full index `fidx`
    /// with the given comma-separated argument list. Imported functions are
    /// dispatched through the injected host; defined ones are (method) calls.
    fn call_expr(&self, fidx: usize, arg_list: &str) -> String {
        if fidx < self.imports.len() {
            format!("self.imports.import{fidx}({arg_list})")
        } else if self.is_method {
            format!("self.func{fidx}({arg_list})")
        } else {
            format!("func{fidx}({arg_list})")
        }
    }
}

/// The borrowed, raw inputs describing one module, as gathered by the parser.
///
/// Bundling them keeps the module-level entry points (`generate_module` and
/// `render_module`) to a small, stable argument list; the derived translation
/// context ([`ModuleCtx`]) is computed from these.
pub(crate) struct ModuleParts<'a> {
    pub(crate) imports: &'a [ImportInfo],
    pub(crate) imported_globals: &'a [ImportedGlobalInfo],
    pub(crate) funcs: &'a [FuncInput<'a>],
    pub(crate) types: &'a [TypeSig],
    pub(crate) globals: &'a [GlobalInfo],
    pub(crate) memory: Option<&'a MemInfo>,
    pub(crate) data: &'a [DataSegment],
    pub(crate) table: Option<&'a TableInfo>,
    pub(crate) elements: &'a [ElemSegment],
}

/// Translate a whole module into Rust source.
///
/// A module that declares linear memory, a table or globals carries mutable
/// state, so it is emitted as a `pub struct Instance` with the functions as
/// `&mut self` methods. A stateless module keeps its functions as free
/// `pub fn`s, matching the earlier phases exactly.
pub(crate) fn generate_module(parts: &ModuleParts<'_>) -> Result<String, TranspileError> {
    let ModuleParts {
        imports,
        imported_globals,
        funcs,
        types,
        globals,
        memory,
        table,
        ..
    } = *parts;

    let has_memory = memory.is_some();
    let has_table = table.is_some();
    // The host is injected whenever anything is imported (functions or globals).
    let has_imports = !imports.is_empty() || !imported_globals.is_empty();
    // Imports must be held by an instance, so a module that has them (or any
    // other mutable state) becomes a `struct Instance` with method functions.
    let stateful = has_memory || has_table || has_imports || !globals.is_empty();

    let ctx = ModuleCtx {
        imports,
        funcs,
        types,
        imported_globals: imported_globals.iter().map(|g| (g.ty, g.mutable)).collect(),
        globals: globals.iter().map(|g| (g.ty, g.mutable)).collect(),
        has_memory,
        has_table,
        is_method: stateful,
    };

    let mut sources = Vec::with_capacity(funcs.len());
    let mut used: HashSet<Helper> = HashSet::new();
    let mut used_rt: HashSet<Rt> = HashSet::new();
    for (index, f) in funcs.iter().enumerate() {
        // Defined functions are named by their full function index, i.e. after
        // the imported functions in the shared index space.
        let generated = generate_function(imports.len() + index, f, &ctx)?;
        used.extend(generated.helpers);
        used_rt.extend(generated.rt);
        sources.push(generated.src);
    }

    // Free-function runtime helpers live at module scope, above the functions
    // (or the `struct Instance`) that call them, in both module shapes.
    let rt_helpers = render_rt_helpers(&used_rt);

    let body = if !stateful {
        sources.join("\n")
    } else {
        render_module(parts, &sources, &used)?
    };

    Ok(if rt_helpers.is_empty() {
        body
    } else {
        format!("{rt_helpers}\n{body}")
    })
}

fn generate_function(
    index: usize,
    input: &FuncInput<'_>,
    ctx: &ModuleCtx<'_>,
) -> Result<GenFn, TranspileError> {
    let mut func = FuncGen::new(input.params, input.results, input.body, ctx)?;
    func.run(input.body)?;
    func.finish(index, input.params, input.results)
}

/// State threaded through the translation of a single function body.
struct FuncGen<'a> {
    local_types: Vec<ValType>,
    mutable_locals: HashSet<u32>,
    /// Module-wide context (functions, types, globals, stateful flags).
    ctx: &'a ModuleCtx<'a>,
    /// The function's result types (0, 1 or more — a tuple when more than one).
    results: Vec<ValType>,
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
    /// Free-function runtime helpers this function relies on.
    used_rt: HashSet<Rt>,
}

impl<'a> FuncGen<'a> {
    fn new(
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
            results: results.to_vec(),
            stack: Vec::new(),
            frames: Vec::new(),
            cur,
            temp_counter: 0,
            label_counter: 0,
            reachable: true,
            dead_nesting: 0,
            trailing: None,
            used_helpers: HashSet::new(),
            used_rt: HashSet::new(),
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
            // `unreachable` always traps; code after it is dead, so stop
            // emitting until the enclosing region ends (as for `return`/`br`).
            Operator::Unreachable => {
                self.line("panic!(\"unreachable\");".to_string());
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
            // produce nothing. `table.fill` is deferred: its value operand is a
            // funcref, which needs reference types.
            Operator::MemoryFill { mem } => self.memory_fill(mem)?,
            Operator::MemoryCopy { dst_mem, src_mem } => self.memory_copy(dst_mem, src_mem)?,
            Operator::TableCopy {
                dst_table,
                src_table,
            } => self.table_copy(dst_table, src_table)?,
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
        // Arithmetic/bitwise results keep the operand type (i32 or i64).
        self.push(Val {
            code: format!("{}.{method}({})", lhs.code, rhs.code),
            ty: lhs.ty,
            stable: lhs.stable && rhs.stable,
        });
        Ok(())
    }

    fn binop_infix(&mut self, op: &str) -> Result<(), TranspileError> {
        let rhs = self.pop()?;
        let lhs = self.pop()?;
        self.push(Val {
            code: format!("({} {op} {})", lhs.code, rhs.code),
            ty: lhs.ty,
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
        // The operands are reinterpreted as the unsigned integer of their width.
        let unsigned = unsigned_type(lhs.ty)?;
        self.push(Val {
            code: format!(
                "i32::from(({} as {unsigned}) {op} ({} as {unsigned}))",
                lhs.code, rhs.code
            ),
            ty: ValType::I32,
            stable: lhs.stable && rhs.stable,
        });
        Ok(())
    }

    /// A shift or rotate: `lhs.method(rhs as u32)`. `wrapping_shl`/`wrapping_shr`
    /// and `rotate_left`/`rotate_right` all take the count mod the bit width, so
    /// this matches wasm's masked shift/rotate count for both i32 and i64.
    fn shift_op(&mut self, method: &str) -> Result<(), TranspileError> {
        let rhs = self.pop()?;
        let lhs = self.pop()?;
        self.push(Val {
            code: format!("{}.{method}({} as u32)", lhs.code, rhs.code),
            ty: lhs.ty,
            stable: lhs.stable && rhs.stable,
        });
        Ok(())
    }

    /// Bind a possibly-trapping expression (integer div/rem) to a temporary at
    /// exactly this program point, so the trap fires in program order and is
    /// not lost if the value is later dropped or skipped by a branch. Pushes
    /// the resulting stable temporary. This mirrors how `call`/`memory_grow`
    /// materialise their side-effecting results.
    fn materialize(&mut self, code: String, ty: ValType) -> Result<(), TranspileError> {
        let name = self.fresh_temp();
        self.line(format!("let {name}: {} = {code};", rust_type(ty)?));
        self.push(Val {
            code: name,
            ty,
            stable: true,
        });
        Ok(())
    }

    /// Signed division. Rust's `/` panics on both a zero divisor and
    /// `iN::MIN / -1`, matching the two wasm `div_s` traps.
    fn div_signed(&mut self) -> Result<(), TranspileError> {
        let rhs = self.pop()?;
        let lhs = self.pop()?;
        self.materialize(format!("{} / {}", lhs.code, rhs.code), lhs.ty)
    }

    /// Signed remainder. `wrapping_rem` panics on a zero divisor and yields 0
    /// for `iN::MIN % -1`, matching wasm `rem_s`.
    fn rem_signed(&mut self) -> Result<(), TranspileError> {
        let rhs = self.pop()?;
        let lhs = self.pop()?;
        self.materialize(format!("{}.wrapping_rem({})", lhs.code, rhs.code), lhs.ty)
    }

    /// Unsigned division (`op` = `/`) or remainder (`op` = `%`): reinterpret both
    /// operands as the unsigned integer of their width, apply `op` (which panics
    /// on a zero divisor), then reinterpret back to the signed type.
    fn div_rem_unsigned(&mut self, op: &str) -> Result<(), TranspileError> {
        let rhs = self.pop()?;
        let lhs = self.pop()?;
        let unsigned = unsigned_type(lhs.ty)?;
        let signed = rust_type(lhs.ty)?;
        self.materialize(
            format!(
                "(({} as {unsigned}) {op} ({} as {unsigned})) as {signed}",
                lhs.code, rhs.code
            ),
            lhs.ty,
        )
    }

    /// A logical (unsigned) shift right: shift the unsigned reinterpretation so
    /// the high bits fill with zero, then reinterpret back to the signed type.
    /// Shifts do not trap, so the value stays a (stable) inline expression.
    fn unsigned_shift(&mut self, method: &str) -> Result<(), TranspileError> {
        let rhs = self.pop()?;
        let lhs = self.pop()?;
        let unsigned = unsigned_type(lhs.ty)?;
        let signed = rust_type(lhs.ty)?;
        self.push(Val {
            code: format!(
                "(({} as {unsigned}).{method}({} as u32) as {signed})",
                lhs.code, rhs.code
            ),
            ty: lhs.ty,
            stable: lhs.stable && rhs.stable,
        });
        Ok(())
    }

    /// A binary call to a free-function runtime helper `name(lhs, rhs)` (used
    /// for float `min`/`max`, whose wasm semantics differ from Rust's). The
    /// helpers are pure, so the result stays stable when both operands are.
    fn call_rt_binop(&mut self, rt: Rt) -> Result<(), TranspileError> {
        let rhs = self.pop()?;
        let lhs = self.pop()?;
        self.used_rt.insert(rt);
        self.push(Val {
            code: format!("{}({}, {})", rt_name(rt), lhs.code, rhs.code),
            ty: lhs.ty,
            stable: lhs.stable && rhs.stable,
        });
        Ok(())
    }

    /// A unary call to a possibly-trapping runtime helper (the non-saturating
    /// float->int truncations, which trap on NaN or an out-of-range operand).
    /// Like div/rem the result is materialised here so the trap fires in
    /// program order rather than being lost if the value is later dropped.
    fn call_rt_unop_trapping(&mut self, rt: Rt, ty: ValType) -> Result<(), TranspileError> {
        let a = self.pop()?;
        self.used_rt.insert(rt);
        self.materialize(format!("{}({})", rt_name(rt), a.code), ty)
    }

    /// A unary method call `operand.method()` (float math like `abs`, `sqrt`).
    fn unop_method(&mut self, method: &str) -> Result<(), TranspileError> {
        let a = self.pop()?;
        self.push(Val {
            code: format!("{}.{method}()", a.code),
            ty: a.ty,
            stable: a.stable,
        });
        Ok(())
    }

    /// Floating-point negation, parenthesised so it composes as a subexpression.
    fn unop_neg(&mut self) -> Result<(), TranspileError> {
        let a = self.pop()?;
        self.push(Val {
            code: format!("(-{})", a.code),
            ty: a.ty,
            stable: a.stable,
        });
        Ok(())
    }

    /// A unary numeric conversion: pop one operand, build the converted
    /// expression from its code via `make`, and push it with `result_ty`. Used
    /// for wrap/extend, int<->float conversions, demote/promote, reinterpret
    /// and the saturating truncations — all pure and non-trapping.
    fn convert(
        &mut self,
        result_ty: ValType,
        make: impl FnOnce(&str) -> String,
    ) -> Result<(), TranspileError> {
        let a = self.pop()?;
        self.push(Val {
            code: make(&a.code),
            ty: result_ty,
            stable: a.stable,
        });
        Ok(())
    }

    /// A single `operand as target` cast (`target` is the Rust primitive name),
    /// pushing the result as `result_ty`.
    fn cast_as(&mut self, result_ty: ValType, target: &str) -> Result<(), TranspileError> {
        self.convert(result_ty, |x| format!("({x} as {target})"))
    }

    /// A cast that first reinterprets `operand as via` and then casts the result
    /// `as target`. Used for unsigned int<->float conversions and byte/half-word
    /// sign extension.
    fn cast_through(
        &mut self,
        result_ty: ValType,
        via: &str,
        target: &str,
    ) -> Result<(), TranspileError> {
        self.convert(result_ty, |x| format!("(({x} as {via}) as {target})"))
    }

    /// A float->int reinterpret: read the operand's raw bits and cast them to the
    /// signed integer `target` of the same width.
    fn reinterpret(&mut self, result_ty: ValType, target: &str) -> Result<(), TranspileError> {
        self.convert(result_ty, |x| format!("({x}.to_bits() as {target})"))
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

    /// Resolve a global index to `(type, mutable, imported)`, spanning imported
    /// globals (the low indices) then defined globals. `imported` is true when
    /// the global is host-backed.
    fn global(&self, global_index: u32) -> Result<(ValType, bool, bool), TranspileError> {
        let n_imported = self.ctx.imported_globals.len();
        let idx = global_index as usize;
        let (entry, imported) = if idx < n_imported {
            (self.ctx.imported_globals.get(idx), true)
        } else {
            (self.ctx.globals.get(idx - n_imported), false)
        };
        entry
            .map(|&(ty, mutable)| (ty, mutable, imported))
            .ok_or_else(|| TranspileError::Unsupported("global index out of range".into()))
    }

    fn global_get(&mut self, global_index: u32) -> Result<(), TranspileError> {
        let (ty, mutable, imported) = self.global(global_index)?;
        // An imported global is fetched from the host and is always unstable (a
        // host getter should not be re-evaluated, so it is materialised when it
        // matters); a defined one is a field, unstable only when mutable.
        let (code, stable) = if imported {
            (format!("self.imports.get_global{global_index}()"), false)
        } else {
            (format!("self.g{global_index}"), !mutable)
        };
        self.push(Val { code, ty, stable });
        Ok(())
    }

    fn global_set(&mut self, global_index: u32) -> Result<(), TranspileError> {
        let (_, mutable, imported) = self.global(global_index)?;
        if !mutable {
            return Err(TranspileError::Unsupported(
                "set of immutable global".into(),
            ));
        }
        self.spill_nonstable()?;
        let value = self.pop()?;
        if imported {
            self.line(format!(
                "self.imports.set_global{global_index}({});",
                value.code
            ));
        } else {
            self.line(format!("self.g{global_index} = {};", value.code));
        }
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

    fn require_table(&self) -> Result<(), TranspileError> {
        if self.ctx.has_table {
            Ok(())
        } else {
            Err(TranspileError::Unsupported(
                "table instruction without a table section".into(),
            ))
        }
    }

    /// Require that a bulk-memory/table operand references memory/table 0, the
    /// only one supported until multi-memory/multi-table lands.
    fn require_zero_index(&self, index: u32, what: &str) -> Result<(), TranspileError> {
        if index == 0 {
            Ok(())
        } else {
            Err(TranspileError::Unsupported(format!(
                "{what} on a non-zero index"
            )))
        }
    }

    fn load(&mut self, helper: Helper, ty: ValType, memarg: MemArg) -> Result<(), TranspileError> {
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
            ty,
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

    /// Pop the three `i32` operands (dest, src/value, len) of a bulk operation,
    /// spilling first since the operation mutates memory/table. Returned in
    /// source order: `(dest, mid, len)`.
    fn pop_bulk_operands(&mut self) -> Result<(Val, Val, Val), TranspileError> {
        self.spill_nonstable()?;
        let len = self.pop()?;
        let mid = self.pop()?;
        let dest = self.pop()?;
        Ok((dest, mid, len))
    }

    fn memory_fill(&mut self, mem: u32) -> Result<(), TranspileError> {
        self.require_memory()?;
        self.require_zero_index(mem, "memory.fill")?;
        let (dest, val, len) = self.pop_bulk_operands()?;
        self.used_helpers.insert(Helper::MemoryFill);
        self.line(format!(
            "self.memory_fill(({}) as u32, {}, ({}) as u32);",
            dest.code, val.code, len.code
        ));
        Ok(())
    }

    fn memory_copy(&mut self, dst_mem: u32, src_mem: u32) -> Result<(), TranspileError> {
        self.require_memory()?;
        self.require_zero_index(dst_mem, "memory.copy")?;
        self.require_zero_index(src_mem, "memory.copy")?;
        let (dest, src, len) = self.pop_bulk_operands()?;
        self.used_helpers.insert(Helper::MemoryCopy);
        self.line(format!(
            "self.memory_copy(({}) as u32, ({}) as u32, ({}) as u32);",
            dest.code, src.code, len.code
        ));
        Ok(())
    }

    fn table_copy(&mut self, dst_table: u32, src_table: u32) -> Result<(), TranspileError> {
        self.require_table()?;
        self.require_zero_index(dst_table, "table.copy")?;
        self.require_zero_index(src_table, "table.copy")?;
        let (dest, src, len) = self.pop_bulk_operands()?;
        self.used_helpers.insert(Helper::TableCopy);
        self.line(format!(
            "self.table_copy(({}) as u32, ({}) as u32, ({}) as u32);",
            dest.code, src.code, len.code
        ));
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
        let (params, results) = self
            .ctx
            .full_sig(function_index as usize)
            .ok_or_else(|| TranspileError::Unsupported("call to unknown function".into()))?;
        let param_count = params.len();
        let results = results.to_vec();

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

        // A call is not re-evaluatable, so bind its result(s) to a temporary at
        // exactly this point (mirroring `memory_grow`) and push the stable
        // temporaries. A multi-value result is destructured from a tuple.
        let call_expr = self.ctx.call_expr(function_index as usize, &arg_list);
        let (prefix, temps) = self.result_binding(&results)?;
        self.line(format!("{prefix}{call_expr};"));
        self.push_temps(temps);
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
        let results = sig.results.clone();
        // The functions any table entry could resolve to: exactly those whose
        // signature equals the declared type (no subtyping, so a structural
        // match is a type match). This spans the whole index space, so a table
        // entry may resolve to an imported function.
        let want = Some((sig.params.as_slice(), sig.results.as_slice()));
        let targets: Vec<usize> = (0..self.ctx.func_count())
            .filter(|&fidx| self.ctx.full_sig(fidx) == want)
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
        // it. The opening line binds the result temporary/tuple (or stands alone
        // as a statement for a result-less call).
        //
        // An out-of-bounds index panics on the slice access (a trap); a null or
        // wrong-type entry falls through to the catch-all panic (also a trap).
        let (prefix, temps) = self.result_binding(&results)?;
        self.line(format!(
            "{prefix}match self.table[({}) as u32 as usize] {{",
            index.code
        ));
        for &fidx in &targets {
            let expr = self.ctx.call_expr(fidx, &arg_list);
            self.line(format!("    {fidx}u32 => {expr},"));
        }
        self.line("    _ => panic!(\"indirect call type mismatch\"),".to_string());
        self.line("};".to_string());
        self.push_temps(temps);
        Ok(())
    }

    /// Build the `let …: … = ` binding prefix for a value producing `results`,
    /// allocating a fresh temporary per result. Returns the prefix (empty for
    /// zero results, so the value is emitted as a bare statement) and the
    /// temporaries to push once the value has been emitted. A multi-value
    /// result binds a tuple pattern.
    fn result_binding(
        &mut self,
        results: &[ValType],
    ) -> Result<(String, Vec<(String, ValType)>), TranspileError> {
        match results {
            [] => Ok((String::new(), Vec::new())),
            [ty] => {
                let name = self.fresh_temp();
                let prefix = format!("let {name}: {} = ", rust_type(*ty)?);
                Ok((prefix, vec![(name, *ty)]))
            }
            many => {
                let tys = rust_types(many)?;
                let names: Vec<String> = many.iter().map(|_| self.fresh_temp()).collect();
                let prefix = format!("let ({}): ({}) = ", names.join(", "), tys.join(", "));
                let temps = names.into_iter().zip(many.iter().copied()).collect();
                Ok((prefix, temps))
            }
        }
    }

    /// Push materialised result temporaries onto the operand stack (stable).
    fn push_temps(&mut self, temps: Vec<(String, ValType)>) {
        for (code, ty) in temps {
            self.push(Val {
                code,
                ty,
                stable: true,
            });
        }
    }

    /// Pop `n` result values (in source order) and format them as a single
    /// expression (`n == 1`) or a tuple (`n > 1`); `None` for `n == 0`.
    fn pop_results(&mut self, n: usize) -> Result<Option<String>, TranspileError> {
        if n == 0 {
            return Ok(None);
        }
        let mut vals = Vec::with_capacity(n);
        for _ in 0..n {
            vals.push(self.pop()?);
        }
        vals.reverse();
        let joined = vals
            .into_iter()
            .map(|v| v.code)
            .collect::<Vec<_>>()
            .join(", ");
        Ok(Some(if n == 1 {
            joined
        } else {
            format!("({joined})")
        }))
    }

    fn emit_return(&mut self) -> Result<(), TranspileError> {
        match self.pop_results(self.results.len())? {
            Some(code) => self.line(format!("return {code};")),
            None => self.line("return;".to_string()),
        }
        self.reachable = false;
        self.dead_nesting = 0;
        Ok(())
    }

    fn end_function(&mut self) -> Result<(), TranspileError> {
        // If control falls off the end, the remaining stack values are the
        // results; otherwise a `return`/`br`/`unreachable` already produced them.
        if self.reachable {
            self.trailing = self.pop_results(self.results.len())?;
        }
        Ok(())
    }

    fn finish(
        self,
        index: usize,
        params: &[ValType],
        results: &[ValType],
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

        let ret = match results {
            [] => String::new(),
            [ty] => format!(" -> {}", rust_type(*ty)?),
            many => format!(" -> ({})", rust_types(many)?.join(", ")),
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
            rt: self.used_rt,
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
        Helper::LoadI64 => "load_i64",
        Helper::LoadF32 => "load_f32",
        Helper::LoadF64 => "load_f64",
        Helper::Load8UI64 => "load8_u_i64",
        Helper::Load8SI64 => "load8_s_i64",
        Helper::Load16UI64 => "load16_u_i64",
        Helper::Load16SI64 => "load16_s_i64",
        Helper::Load32UI64 => "load32_u_i64",
        Helper::Load32SI64 => "load32_s_i64",
        Helper::StoreI32 => "store_i32",
        Helper::Store8 => "store8",
        Helper::Store16 => "store16",
        Helper::StoreI64 => "store_i64",
        Helper::StoreF32 => "store_f32",
        Helper::StoreF64 => "store_f64",
        Helper::Store8I64 => "store8_i64",
        Helper::Store16I64 => "store16_i64",
        Helper::Store32I64 => "store32_i64",
        Helper::Grow => "memory_grow",
        Helper::MemoryFill => "memory_fill",
        Helper::MemoryCopy => "memory_copy",
        Helper::TableCopy => "table_copy",
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

/// The Rust type name of each value type, in order.
fn rust_types(tys: &[ValType]) -> Result<Vec<&'static str>, TranspileError> {
    tys.iter().map(|ty| rust_type(*ty)).collect()
}

/// The unsigned integer type used to reinterpret `ty` for unsigned operations.
fn unsigned_type(ty: ValType) -> Result<&'static str, TranspileError> {
    match ty {
        ValType::I32 => Ok("u32"),
        ValType::I64 => Ok("u64"),
        other => Err(TranspileError::Unsupported(format!(
            "unsigned operation on {other:?}"
        ))),
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
const HELPER_ORDER: [Helper; 27] = [
    Helper::LoadI32,
    Helper::Load8U,
    Helper::Load8S,
    Helper::Load16U,
    Helper::Load16S,
    Helper::LoadI64,
    Helper::LoadF32,
    Helper::LoadF64,
    Helper::Load8UI64,
    Helper::Load8SI64,
    Helper::Load16UI64,
    Helper::Load16SI64,
    Helper::Load32UI64,
    Helper::Load32SI64,
    Helper::StoreI32,
    Helper::Store8,
    Helper::Store16,
    Helper::StoreI64,
    Helper::StoreF32,
    Helper::StoreF64,
    Helper::Store8I64,
    Helper::Store16I64,
    Helper::Store32I64,
    Helper::Grow,
    Helper::MemoryFill,
    Helper::MemoryCopy,
    Helper::TableCopy,
];

/// Render the `struct Instance` and its `impl` for a stateful module. When the
/// module imports functions, a `pub trait Imports` is emitted and `Instance`
/// becomes generic over a host `H: Imports` that it stores and dispatches to.
fn render_module(
    parts: &ModuleParts<'_>,
    sources: &[String],
    used: &HashSet<Helper>,
) -> Result<String, TranspileError> {
    let ModuleParts {
        imports,
        imported_globals,
        globals,
        memory,
        data,
        table,
        elements,
        ..
    } = *parts;

    // Defined globals are named by their full index, i.e. after the imported
    // globals in the shared global index space.
    let global_base = imported_globals.len();

    let mut lines: Vec<String> = Vec::new();

    let has_imports = !imports.is_empty() || !imported_globals.is_empty();
    if has_imports {
        lines.extend(import_trait_lines(imports, imported_globals)?);
        lines.push(String::new());
    }
    // A generic parameter carries the host implementation only when needed:
    // `<H: Imports>` where the parameter is bound (struct/impl header) and
    // `<H>` where the type is merely named (`Instance<H>`).
    let (decl_generics, type_generics) = if has_imports {
        ("<H: Imports>", "<H>")
    } else {
        ("", "")
    };

    lines.push("#[allow(dead_code)]".to_string());
    lines.push(format!("pub struct Instance{decl_generics} {{"));
    if has_imports {
        lines.push("    imports: H,".to_string());
    }
    if memory.is_some() {
        lines.push("    memory: Vec<u8>,".to_string());
    }
    if table.is_some() {
        // A table entry is a function index; `u32::MAX` marks a null funcref.
        lines.push("    table: Vec<u32>,".to_string());
    }
    for (i, g) in globals.iter().enumerate() {
        lines.push(format!("    g{}: {},", global_base + i, rust_type(g.ty)?));
    }
    lines.push("}".to_string());
    lines.push(String::new());

    lines.push(ALLOW.to_string());
    lines.push(format!("impl{decl_generics} Instance{type_generics} {{"));

    // Everything inside the `impl` is collected unindented, then indented once.
    let mut inner: Vec<String> = Vec::new();
    let new_param = if has_imports { "imports: H" } else { "" };
    inner.push(format!("pub fn new({new_param}) -> Self {{"));
    inner.push("    Self {".to_string());
    if has_imports {
        inner.push("        imports,".to_string());
    }
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
        inner.push(format!("        g{}: {},", global_base + i, g.init));
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

/// The `pub trait Imports` declaration: one `import{j}` method per imported
/// function, taking `&mut self` since a host call may have side effects.
fn import_trait_lines(
    imports: &[ImportInfo],
    imported_globals: &[ImportedGlobalInfo],
) -> Result<Vec<String>, TranspileError> {
    let mut lines = vec!["pub trait Imports {".to_string()];
    for (j, im) in imports.iter().enumerate() {
        let mut params = String::from("&mut self");
        for (k, ty) in im.params.iter().enumerate() {
            params.push_str(&format!(", a{k}: {}", rust_type(*ty)?));
        }
        let ret = match im.results.as_slice() {
            [] => String::new(),
            [ty] => format!(" -> {}", rust_type(*ty)?),
            _ => {
                return Err(TranspileError::Unsupported(
                    "multi-value import result".into(),
                ));
            }
        };
        lines.push(format!("    fn import{j}({params}){ret};"));
    }
    // Each imported global gets a getter; a mutable one also gets a setter.
    for (k, g) in imported_globals.iter().enumerate() {
        let ty = rust_type(g.ty)?;
        lines.push(format!("    fn get_global{k}(&self) -> {ty};"));
        if g.mutable {
            lines.push(format!("    fn set_global{k}(&mut self, v: {ty});"));
        }
    }
    lines.push("}".to_string());
    Ok(lines)
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
            "    self.memory[a..a + 4].copy_from_slice(&value.to_le_bytes());",
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
            "    self.memory[a..a + 2].copy_from_slice(&(value as u16).to_le_bytes());",
            "}",
        ]),
        Helper::LoadI64 => owned(&[
            "fn load_i64(&self, addr: u32, offset: u32) -> i64 {",
            "    let a = addr as usize + offset as usize;",
            "    i64::from_le_bytes([self.memory[a], self.memory[a + 1], self.memory[a + 2], self.memory[a + 3], self.memory[a + 4], self.memory[a + 5], self.memory[a + 6], self.memory[a + 7]])",
            "}",
        ]),
        Helper::LoadF32 => owned(&[
            "fn load_f32(&self, addr: u32, offset: u32) -> f32 {",
            "    let a = addr as usize + offset as usize;",
            "    f32::from_le_bytes([self.memory[a], self.memory[a + 1], self.memory[a + 2], self.memory[a + 3]])",
            "}",
        ]),
        Helper::LoadF64 => owned(&[
            "fn load_f64(&self, addr: u32, offset: u32) -> f64 {",
            "    let a = addr as usize + offset as usize;",
            "    f64::from_le_bytes([self.memory[a], self.memory[a + 1], self.memory[a + 2], self.memory[a + 3], self.memory[a + 4], self.memory[a + 5], self.memory[a + 6], self.memory[a + 7]])",
            "}",
        ]),
        Helper::Load8UI64 => owned(&[
            "fn load8_u_i64(&self, addr: u32, offset: u32) -> i64 {",
            "    let a = addr as usize + offset as usize;",
            "    self.memory[a] as i64",
            "}",
        ]),
        Helper::Load8SI64 => owned(&[
            "fn load8_s_i64(&self, addr: u32, offset: u32) -> i64 {",
            "    let a = addr as usize + offset as usize;",
            "    self.memory[a] as i8 as i64",
            "}",
        ]),
        Helper::Load16UI64 => owned(&[
            "fn load16_u_i64(&self, addr: u32, offset: u32) -> i64 {",
            "    let a = addr as usize + offset as usize;",
            "    u16::from_le_bytes([self.memory[a], self.memory[a + 1]]) as i64",
            "}",
        ]),
        Helper::Load16SI64 => owned(&[
            "fn load16_s_i64(&self, addr: u32, offset: u32) -> i64 {",
            "    let a = addr as usize + offset as usize;",
            "    i16::from_le_bytes([self.memory[a], self.memory[a + 1]]) as i64",
            "}",
        ]),
        Helper::Load32UI64 => owned(&[
            "fn load32_u_i64(&self, addr: u32, offset: u32) -> i64 {",
            "    let a = addr as usize + offset as usize;",
            "    u32::from_le_bytes([self.memory[a], self.memory[a + 1], self.memory[a + 2], self.memory[a + 3]]) as i64",
            "}",
        ]),
        Helper::Load32SI64 => owned(&[
            "fn load32_s_i64(&self, addr: u32, offset: u32) -> i64 {",
            "    let a = addr as usize + offset as usize;",
            "    i32::from_le_bytes([self.memory[a], self.memory[a + 1], self.memory[a + 2], self.memory[a + 3]]) as i64",
            "}",
        ]),
        Helper::StoreI64 => owned(&[
            "fn store_i64(&mut self, addr: u32, offset: u32, value: i64) {",
            "    let a = addr as usize + offset as usize;",
            "    self.memory[a..a + 8].copy_from_slice(&value.to_le_bytes());",
            "}",
        ]),
        Helper::StoreF32 => owned(&[
            "fn store_f32(&mut self, addr: u32, offset: u32, value: f32) {",
            "    let a = addr as usize + offset as usize;",
            "    self.memory[a..a + 4].copy_from_slice(&value.to_le_bytes());",
            "}",
        ]),
        Helper::StoreF64 => owned(&[
            "fn store_f64(&mut self, addr: u32, offset: u32, value: f64) {",
            "    let a = addr as usize + offset as usize;",
            "    self.memory[a..a + 8].copy_from_slice(&value.to_le_bytes());",
            "}",
        ]),
        Helper::Store8I64 => owned(&[
            "fn store8_i64(&mut self, addr: u32, offset: u32, value: i64) {",
            "    let a = addr as usize + offset as usize;",
            "    self.memory[a] = value as u8;",
            "}",
        ]),
        Helper::Store16I64 => owned(&[
            "fn store16_i64(&mut self, addr: u32, offset: u32, value: i64) {",
            "    let a = addr as usize + offset as usize;",
            "    self.memory[a..a + 2].copy_from_slice(&(value as u16).to_le_bytes());",
            "}",
        ]),
        Helper::Store32I64 => owned(&[
            "fn store32_i64(&mut self, addr: u32, offset: u32, value: i64) {",
            "    let a = addr as usize + offset as usize;",
            "    self.memory[a..a + 4].copy_from_slice(&(value as u32).to_le_bytes());",
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
        // Bulk operations. An out-of-bounds range panics on the slice access or
        // `copy_within` (a wasm trap); `copy_within` is memmove, so overlapping
        // source and destination copy correctly.
        Helper::MemoryFill => owned(&[
            "fn memory_fill(&mut self, dest: u32, val: i32, len: u32) {",
            "    let d = dest as usize;",
            "    self.memory[d..d + len as usize].fill(val as u8);",
            "}",
        ]),
        Helper::MemoryCopy => owned(&[
            "fn memory_copy(&mut self, dest: u32, src: u32, len: u32) {",
            "    let s = src as usize;",
            "    let d = dest as usize;",
            "    self.memory.copy_within(s..s + len as usize, d);",
            "}",
        ]),
        Helper::TableCopy => owned(&[
            "fn table_copy(&mut self, dest: u32, src: u32, len: u32) {",
            "    let s = src as usize;",
            "    let d = dest as usize;",
            "    self.table.copy_within(s..s + len as usize, d);",
            "}",
        ]),
    }
}

fn rt_name(rt: Rt) -> &'static str {
    match rt {
        Rt::F32Min => "f32_min",
        Rt::F32Max => "f32_max",
        Rt::F64Min => "f64_min",
        Rt::F64Max => "f64_max",
        Rt::I32TruncF32S => "i32_trunc_f32_s",
        Rt::I32TruncF32U => "i32_trunc_f32_u",
        Rt::I32TruncF64S => "i32_trunc_f64_s",
        Rt::I32TruncF64U => "i32_trunc_f64_u",
        Rt::I64TruncF32S => "i64_trunc_f32_s",
        Rt::I64TruncF32U => "i64_trunc_f32_u",
        Rt::I64TruncF64S => "i64_trunc_f64_s",
        Rt::I64TruncF64U => "i64_trunc_f64_u",
    }
}

/// All runtime free-function helpers, in a deterministic emission order.
const RT_ORDER: [Rt; 12] = [
    Rt::F32Min,
    Rt::F32Max,
    Rt::F64Min,
    Rt::F64Max,
    Rt::I32TruncF32S,
    Rt::I32TruncF32U,
    Rt::I32TruncF64S,
    Rt::I32TruncF64U,
    Rt::I64TruncF32S,
    Rt::I64TruncF32U,
    Rt::I64TruncF64S,
    Rt::I64TruncF64U,
];

/// Render the used runtime helpers as module-scope free functions, in
/// [`RT_ORDER`], separated by blank lines. Returns an empty string if none.
fn render_rt_helpers(used: &HashSet<Rt>) -> String {
    let mut blocks: Vec<String> = Vec::new();
    for rt in RT_ORDER {
        if used.contains(&rt) {
            blocks.push(rt_lines(rt).join("\n"));
        }
    }
    let mut out = blocks.join("\n\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

/// The source lines of one runtime helper, dispatched to the min/max or the
/// trapping-truncation template.
fn rt_lines(rt: Rt) -> Vec<String> {
    match rt {
        Rt::F32Min | Rt::F32Max | Rt::F64Min | Rt::F64Max => rt_minmax_lines(rt),
        Rt::I32TruncF32S => {
            trunc_lines("i32_trunc_f32_s", "f32", "i32", TRUNC_I32_S_F32, "x as i32")
        }
        Rt::I32TruncF32U => trunc_lines(
            "i32_trunc_f32_u",
            "f32",
            "i32",
            TRUNC_U_F32_32,
            "x as u32 as i32",
        ),
        Rt::I32TruncF64S => {
            trunc_lines("i32_trunc_f64_s", "f64", "i32", TRUNC_I32_S_F64, "x as i32")
        }
        Rt::I32TruncF64U => trunc_lines(
            "i32_trunc_f64_u",
            "f64",
            "i32",
            TRUNC_U_F64_32,
            "x as u32 as i32",
        ),
        Rt::I64TruncF32S => {
            trunc_lines("i64_trunc_f32_s", "f32", "i64", TRUNC_I64_S_F32, "x as i64")
        }
        Rt::I64TruncF32U => trunc_lines(
            "i64_trunc_f32_u",
            "f32",
            "i64",
            TRUNC_U_F32_64,
            "x as u64 as i64",
        ),
        Rt::I64TruncF64S => {
            trunc_lines("i64_trunc_f64_s", "f64", "i64", TRUNC_I64_S_F64, "x as i64")
        }
        Rt::I64TruncF64U => trunc_lines(
            "i64_trunc_f64_u",
            "f64",
            "i64",
            TRUNC_U_F64_64,
            "x as u64 as i64",
        ),
    }
}

// The in-range predicates for the trapping truncations, following wasm2c's
// proven bounds. Signed f32 sources use `>=` on the exact `-2^N` lower bound
// (the next representable f32 below already truncates out of range); signed
// i32-from-f64 needs a strict `> -2^31 - 1` because f64 can represent values
// between `-2^31 - 1` and `-2^31`. Unsigned sources reject anything `<= -1`.
const TRUNC_I32_S_F32: &str = "x >= -2147483648.0f32 && x < 2147483648.0f32";
const TRUNC_I32_S_F64: &str = "x > -2147483649.0f64 && x < 2147483648.0f64";
const TRUNC_I64_S_F32: &str = "x >= -9223372036854775808.0f32 && x < 9223372036854775808.0f32";
const TRUNC_I64_S_F64: &str = "x >= -9223372036854775808.0f64 && x < 9223372036854775808.0f64";
const TRUNC_U_F32_32: &str = "x > -1.0f32 && x < 4294967296.0f32";
const TRUNC_U_F64_32: &str = "x > -1.0f64 && x < 4294967296.0f64";
const TRUNC_U_F32_64: &str = "x > -1.0f32 && x < 18446744073709551616.0f32";
const TRUNC_U_F64_64: &str = "x > -1.0f64 && x < 18446744073709551616.0f64";

/// A non-saturating float->int truncation helper: trap on NaN, trap when the
/// value is outside `range`, else convert via `cast`.
fn trunc_lines(name: &str, ft: &str, it: &str, range: &str, cast: &str) -> Vec<String> {
    vec![
        format!("fn {name}(x: {ft}) -> {it} {{"),
        "    if x.is_nan() {".to_string(),
        "        panic!(\"invalid conversion to integer\");".to_string(),
        "    }".to_string(),
        format!("    if !({range}) {{"),
        "        panic!(\"integer overflow\");".to_string(),
        "    }".to_string(),
        format!("    {cast}"),
        "}".to_string(),
    ]
}

/// wasm `min`/`max` return NaN if either operand is NaN, and when the operands
/// are equal (notably ±0) `min` yields the negatively-signed and `max` the
/// positively-signed value — differing from Rust's `f32::min`/`max`.
fn rt_minmax_lines(rt: Rt) -> Vec<String> {
    let owned = |lines: &[&str]| lines.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    let name = rt_name(rt);
    let ty = if matches!(rt, Rt::F32Min | Rt::F32Max) {
        "f32"
    } else {
        "f64"
    };
    // For equal operands, `min` keeps the negative one and `max` the positive
    // one; the `<`/`>` picks the smaller/larger otherwise.
    let (equal_pick, order_op) = if matches!(rt, Rt::F32Min | Rt::F64Min) {
        ("if a.is_sign_negative() { a } else { b }", "<")
    } else {
        ("if a.is_sign_negative() { b } else { a }", ">")
    };
    owned(&[
        &format!("fn {name}(a: {ty}, b: {ty}) -> {ty} {{"),
        "    if a.is_nan() || b.is_nan() {",
        &format!("        return {ty}::NAN;"),
        "    }",
        "    if a == b {",
        &format!("        {equal_pick}"),
        &format!("    }} else if a {order_op} b {{"),
        "        a",
        "    } else {",
        "        b",
        "    }",
        "}",
    ])
}
