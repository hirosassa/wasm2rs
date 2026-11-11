//! Per-function code generation.
//!
//! wasm bytecode is already structured — `block`/`loop`/`if` regions nest
//! properly and `br N` can only target one of the `N` enclosing labels, so
//! there is never an irreducible control-flow graph. Each region therefore
//! maps directly onto Rust: a `block` or `loop` that is actually branched to
//! becomes a labelled `loop { ... }` (with `break`/`continue` for `br`), while
//! one that is never targeted is emitted inline to avoid unused-label warnings.
//!
//! A function whose control nesting exceeds [`FLATTEN_DEPTH_THRESHOLD`] is
//! instead emitted as a flat `loop { match pc { … } }` dispatch (see
//! [`flatten_body`]), so its rendered nesting is a small constant and cannot
//! overflow rustc's recursive-descent parser. Ordinary functions keep the
//! readable nested form above, byte for byte.
//!
//! Values are tracked on a simulated operand stack of expression strings. A
//! value is "stable" when re-evaluating its expression always yields the same
//! result (a constant, an immutable local, a materialised temporary, or a
//! combination of those). Only non-stable values are spilled into `let`
//! bindings at control-flow boundaries and before local mutations, which keeps
//! straight-line code compiling to clean inline expressions.

use std::collections::HashSet;

use wasmparser::{MemArg, ValType};

use crate::TranspileError;

mod const_expr;
mod export_names;
mod func;
mod helpers;
mod info;
mod parallel;
mod render;
mod render_cont;
mod runtime;
mod simd_rt;
mod wasi;

use self::func::FuncGen;
use self::render::{chunk_prelude, render_lib_root};
use self::runtime::{render_rt_helpers, rt_name};
use self::simd_rt::render_simd_helpers;

pub(crate) use self::const_expr::{
    ConstInt, IntGlobals, const_expr_to_rust, const_expr_u32, const_expr_u64, const_expr_u64_for,
    const_int_value,
};
pub(crate) use self::export_names::{
    assign as assign_export_names, assign_import_names, interface_ident, plain_ident,
};
pub(crate) use self::info::{
    CompositeKind, DataSegment, ElemSegment, ExportInfo, FieldInfo, FuncInput, GlobalExportInfo,
    GlobalInfo, ImportInfo, ImportedGlobalInfo, MemInfo, SegOffset, TableFill, TableInfo, TagInfo,
    TypeRel, TypeSig, WASM_MAX_PAGES, WASM_PAGE_SIZE, is_gc_table_element, table_elem_rust,
    table_null,
};
pub(crate) use self::reach::{ReachInput, live_functions};
pub(crate) use self::wasi::WasiFn;

mod analysis;
mod driver;
mod flatten;
mod ir;
mod reach;

pub(crate) use self::analysis::*;
pub(crate) use self::driver::*;
pub(crate) use self::flatten::*;
pub(crate) use self::ir::*;

/// A memory-access helper method emitted on the instance `impl` on demand.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Helper {
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
    LoadV128,
    StoreV128,
    Load8Splat,
    Load16Splat,
    Load32Splat,
    Load64Splat,
    Load32Zero,
    Load64Zero,
    Load8x8S,
    Load8x8U,
    Load16x4S,
    Load16x4U,
    Load32x2S,
    Load32x2U,
    Load8Lane,
    Load16Lane,
    Load32Lane,
    Load64Lane,
    Store8Lane,
    Store16Lane,
    Store32Lane,
    Store64Lane,
    Grow,
    MemoryFill,
    MemoryCopy,
    TableCopy,
    TableFill,
}
/// A free-standing runtime helper function emitted at module scope on demand.
/// Unlike [`Helper`], these do not touch instance state (memory/globals), so
/// they are plain `fn`s usable from both stateless and stateful modules — used
/// for operations whose wasm semantics differ from Rust's built-in operators.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Rt {
    F32Min,
    F32Max,
    F64Min,
    F64Max,
    QuietF32,
    QuietF64,
    I32TruncF32S,
    I32TruncF32U,
    I32TruncF64S,
    I32TruncF64U,
    I64TruncF32S,
    I64TruncF32U,
    I64TruncF64S,
    I64TruncF64U,
    // SIMD lane-splat helpers: broadcast a scalar into every lane of a v128.
    SplatI8x16,
    SplatI16x8,
    SplatI32x4,
    SplatI64x2,
    SplatF32x4,
    SplatF64x2,
    /// The `unreachable` trap, emitted as a `#[cold] #[inline(never)]` helper so
    /// its `panic!` is out-of-line: one call site per `unreachable` instead of a
    /// panic expansion inlined into every hot body.
    TrapUnreachable,
    /// The `call_indirect` type-mismatch trap, emitted as a `#[cold]
    /// #[inline(never)]` helper so the panic stays out-of-line even when the
    /// dispatch method is inlined onto the hot indirect-call path.
    TrapIndirectTypeMismatch,
    /// The tail-call trampoline slot: a `return_call*` parks the callee here and
    /// returns, and the caller's wrapper runs it so the stack stays constant.
    Tc,
    /// The per-thread activation counter behind `TranspileOptions::call_depth_limit`:
    /// `CallDepth::enter()` bumps it (panicking with `call stack exhausted` past the
    /// limit) and its `Drop` restores it, so unwinding keeps the count exact.
    CallDepth,
    /// The out-of-line `call stack exhausted` trap `CallDepth::enter` raises.
    TrapCallStackExhausted,
    /// The wrapper a `delegate` raises when it must bypass enclosing `try`
    /// bodies: the exception plus how many more bodies to skip.
    DelegateExc,
}
/// The helper dependencies discovered while generating one function.
///
/// The function's own Rust source is written straight into the caller's output
/// buffer (see [`generate_function_into`]) rather than returned, so the whole
/// body is never held twice; only these aggregated sets — needed to render the
/// module/root once every function has been seen — are returned.
pub(crate) struct GenMeta {
    /// Instance-method memory helpers, each tagged with the linear memory index
    /// it acts on (0 for the historic single-memory helpers, `i` for `_m{i}`).
    pub(crate) helpers: HashSet<(Helper, u32)>,
    /// Module-scope free-function runtime helpers.
    pub(crate) rt: HashSet<Rt>,
    /// Module-scope lane-wise SIMD helpers, tracked by function name.
    pub(crate) simd: HashSet<&'static str>,
    /// `call_indirect` type indices needing a `call_ref_t{ti}` dispatch method.
    pub(crate) dispatch_sigs: HashSet<u32>,
    /// Whether the function uses legacy exception handling, so the module needs
    /// the [`EXC_TYPE`] definition.
    pub(crate) uses_eh: bool,
    /// Shared state-struct definitions produced by splitting a large flattened
    /// method's dispatch (see [`SplitPlan`]); emitted at the crate root so every
    /// chunk's `use super::*` resolves them. Empty for a free function (whose
    /// struct is emitted inline) or an un-split function.
    pub(crate) state_structs: Vec<String>,
}
/// The lint-suppression attribute prefixed to generated functions/impls.
pub(crate) const ALLOW: &str = "#[allow(dead_code, unused_variables, unused_assignments, unused_mut, unused_parens, unconditional_recursion, unconditional_panic)]";
/// The module-scope type name carrying a thrown wasm exception's tag and its
/// (bit-encoded) payload values.
const EXC_TYPE: &str = "Wasm2RsException";
/// The module-scope definition of [`EXC_TYPE`], emitted once when any function
/// uses exception handling. A thrown exception is raised with `resume_unwind`
/// (so the panic hook never runs) as this type; each payload value is
/// bit-encoded into a `u64` so one field carries any mix of numeric types.
const EXC_DEF: &str = "\
#[allow(dead_code)]
#[derive(Clone)]
struct Wasm2RsException {
    tag: u32,
    values: Vec<u64>,
    // The `exnref` payload values: `values` holds an index into this vector for
    // each of them (`None` is a null reference).
    exns: Vec<Option<std::sync::Arc<dyn std::any::Any + Send + Sync>>>,
}";
/// The `exnref` helpers: an `exnref` is a managed `GcRef::Exn`, so they exist only
/// when both [`EXC_DEF`] and [`GCREF_DEF`] are emitted.
const EXN_REF_DEF: &str = "\
// An `exnref` is a `GcRef::Exn` sharing the caught exception, so the exception is
// freed once the last reference to it is dropped. It is an `Arc` (not an `Rc`)
// because the exception itself is the payload of a panic, which must be `Send`.
#[allow(dead_code)]
fn wasm2rs_exn_new(exn: &Wasm2RsException) -> GcRef {
    GcRef::Exn(std::sync::Arc::new(exn.clone()))
}

#[allow(dead_code)]
fn wasm2rs_exn_load(exn: &GcRef) -> Wasm2RsException {
    match exn {
        GcRef::Exn(e) => match e.downcast_ref::<Wasm2RsException>() {
            Some(exn) => exn.clone(),
            None => panic!(\"not an exception reference\"),
        },
        _ => panic!(\"null exception reference\"),
    }
}

#[allow(dead_code)]
fn wasm2rs_exn_pack(exns: &mut Vec<Option<std::sync::Arc<dyn std::any::Any + Send + Sync>>>, exn: GcRef) -> u64 {
    exns.push(match exn {
        GcRef::Exn(e) => Some(e),
        _ => None,
    });
    (exns.len() - 1) as u64
}

#[allow(dead_code)]
fn wasm2rs_exn_unpack(exns: &[Option<std::sync::Arc<dyn std::any::Any + Send + Sync>>], index: u64) -> GcRef {
    match &exns[index as usize] {
        Some(e) => GcRef::Exn(e.clone()),
        None => GcRef::Null,
    }
}";
/// The managed value model for heap-allocated `struct`/`array` objects (GC
/// phase 4b). Every managed object — struct or array alike — is an
/// `Rc<RefCell<Vec<GcSlot>>>`: a struct's slots are its fields in declaration
/// order, an array's slots are its elements. `GcRef::Null` is the null
/// reference. Reference cycles leak, since there is no tracing collector; that
/// is acceptable for this phase. Emitted at module scope only when the module
/// declares at least one struct/array type.
const GCREF_DEF: &str = "\
#[derive(Clone)]
#[allow(dead_code)]
pub enum GcRef {
    Null,
    I31(i32),
    Obj { ty: u32, slots: std::rc::Rc<std::cell::RefCell<Vec<GcSlot>>> },
    Exn(std::sync::Arc<dyn std::any::Any + Send + Sync>),
}

#[derive(Clone)]
#[allow(dead_code)]
pub enum GcSlot {
    I32(i32),
    I64(i64),
    F32(f32),
    F64(f64),
    V128(u128),
    Ref(GcRef),
    Func(u32),
}

#[allow(dead_code)]
impl GcRef {
    fn obj(&self) -> std::rc::Rc<std::cell::RefCell<Vec<GcSlot>>> {
        match self {
            GcRef::Obj { slots, .. } => slots.clone(),
            GcRef::Null | GcRef::I31(_) | GcRef::Exn(_) => panic!(\"null reference\"),
        }
    }
}";
/// The Rust expression bit-encoding a payload operand of type `ty` (given as the
/// expression `expr`) into the `u64` stored in an exception's `values`.
fn encode_exc_value(
    ty: ValType,
    expr: &str,
    kinds: &[CompositeKind],
) -> Result<String, TranspileError> {
    Ok(match ty {
        ValType::I32 => format!("({expr}) as u32 as u64"),
        ValType::I64 => format!("({expr}) as u64"),
        ValType::F32 => format!("({expr}).to_bits() as u64"),
        ValType::F64 => format!("({expr}).to_bits()"),
        // A `u32`-backed handle (e.g. `funcref`/`externref`/`exnref`) rides as its
        // value; a managed `GcRef` cannot.
        ValType::Ref(_) if is_u32_ref(ty, kinds)? => format!("({expr}) as u64"),
        // An `exnref` rides as an index into the exception's `exns` side vector
        // (`__exns`, declared by the throw site).
        ValType::Ref(rt) if is_exn_hierarchy(rt.heap_type()) => {
            format!("wasm2rs_exn_pack(&mut __exns, ({expr}).clone())")
        }
        ValType::Ref(_) => {
            return Err(TranspileError::Unsupported(
                "exception payload with a reference type".into(),
            ));
        }
        ValType::V128 => {
            return Err(TranspileError::Unsupported(
                "exception payload with a v128 value".into(),
            ));
        }
    })
}
/// The Rust expression decoding a `u64` (given as `expr`) from an exception's
/// `values` back into an operand of type `ty`, inverting [`encode_exc_value`].
/// `exns` is the expression for the exception's `exns` vector, which an `exnref`
/// value indexes.
fn decode_exc_value(
    ty: ValType,
    expr: &str,
    exns: &str,
    kinds: &[CompositeKind],
) -> Result<String, TranspileError> {
    Ok(match ty {
        ValType::I32 => format!("{expr} as u32 as i32"),
        ValType::I64 => format!("{expr} as i64"),
        ValType::F32 => format!("f32::from_bits({expr} as u32)"),
        ValType::F64 => format!("f64::from_bits({expr})"),
        ValType::Ref(_) if is_u32_ref(ty, kinds)? => format!("{expr} as u32"),
        ValType::Ref(rt) if is_exn_hierarchy(rt.heap_type()) => {
            format!("wasm2rs_exn_unpack(&{exns}, {expr})")
        }
        ValType::Ref(_) => {
            return Err(TranspileError::Unsupported(
                "exception payload with a reference type".into(),
            ));
        }
        ValType::V128 => {
            return Err(TranspileError::Unsupported(
                "exception payload with a v128 value".into(),
            ));
        }
    })
}
/// The offset field of a memory access, as a `u32` (32-bit memory only).
fn memarg_offset(memarg: MemArg) -> Result<u32, TranspileError> {
    u32::try_from(memarg.offset)
        .map_err(|_| TranspileError::Unsupported("memory offset too large".into()))
}
/// Append bytes to `out` as the body of a byte-string literal (without the
/// `b"` quotes).
///
/// A segment is one string literal rather than an array of per-byte
/// expressions, which keeps the source small and spares rustc one AST node per
/// byte. The literal is broken with `\` line continuations every `PER_LINE`
/// bytes so no source line grows to megabytes. A continuation skips the next
/// line's leading whitespace, so a space at the start of a line is escaped.
fn byte_string_literal(out: &mut String, bytes: &[u8]) {
    const PER_LINE: usize = 64;
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for (i, &b) in bytes.iter().enumerate() {
        let line_start = i % PER_LINE == 0;
        if line_start && i > 0 {
            out.push_str("\\\n");
        }
        let plain = b.is_ascii_graphic() && b != b'"' && b != b'\\';
        if plain || (b == b' ' && !line_start) {
            out.push(char::from(b));
        } else {
            out.push_str("\\x");
            out.push(char::from(HEX[usize::from(b >> 4)]));
            out.push(char::from(HEX[usize::from(b & 15)]));
        }
    }
}
/// A module-scope `static DATA{d}` holding one data segment's bytes.
fn data_static_line(d: usize, bytes: &[u8]) -> String {
    // Worst case: 4 characters per byte plus a continuation per line.
    let mut line = String::with_capacity(bytes.len().saturating_mul(4).saturating_add(64));
    line.push_str(&format!("static DATA{d}: [u8; {}] = *b\"", bytes.len()));
    byte_string_literal(&mut line, bytes);
    line.push_str("\";");
    line
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
