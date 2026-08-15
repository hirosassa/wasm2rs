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
mod func;
mod helpers;
mod info;
mod render;
mod render_cont;
mod runtime;
mod simd_rt;
mod wasi;

use self::func::FuncGen;
use self::render::{chunk_prelude, render_lib_root, render_module};
use self::runtime::{render_rt_helpers, rt_name};
use self::simd_rt::render_simd_helpers;

pub(crate) use self::const_expr::{const_expr_to_rust, const_expr_u32};
pub(crate) use self::info::{
    CompositeKind, DataSegment, ElemSegment, FieldInfo, FuncInput, GlobalInfo, ImportInfo,
    ImportedGlobalInfo, MemInfo, TableInfo, TagInfo, TypeSig, WASM_MAX_PAGES, WASM_PAGE_SIZE,
};
pub(crate) use self::wasi::WasiFn;

mod analysis;
mod driver;
mod flatten;
mod ir;

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
const ALLOW: &str =
    "#[allow(dead_code, unused_variables, unused_assignments, unused_mut, unused_parens)]";
/// The module-scope type name carrying a thrown wasm exception's tag and its
/// (bit-encoded) payload values.
const EXC_TYPE: &str = "Wasm2RsException";
/// The module-scope definition of [`EXC_TYPE`], emitted once when any function
/// uses exception handling. A thrown exception is `panic_any`-ed as this type;
/// each payload value is bit-encoded into a `u64` so one field carries any mix
/// of numeric types.
const EXC_DEF: &str = "\
#[allow(dead_code)]
struct Wasm2RsException {
    tag: u32,
    values: Vec<u64>,
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
            GcRef::Null | GcRef::I31(_) => panic!(\"null reference\"),
        }
    }
}";
/// The Rust expression bit-encoding a payload operand of type `ty` (given as the
/// expression `expr`) into the `u64` stored in an exception's `values`.
fn encode_exc_value(ty: ValType, expr: &str) -> Result<String, TranspileError> {
    Ok(match ty {
        ValType::I32 => format!("({expr}) as u32 as u64"),
        ValType::I64 => format!("({expr}) as u64"),
        ValType::F32 => format!("({expr}).to_bits() as u64"),
        ValType::F64 => format!("({expr}).to_bits()"),
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
fn decode_exc_value(ty: ValType, expr: &str) -> Result<String, TranspileError> {
    Ok(match ty {
        ValType::I32 => format!("{expr} as u32 as i32"),
        ValType::I64 => format!("{expr} as i64"),
        ValType::F32 => format!("f32::from_bits({expr} as u32)"),
        ValType::F64 => format!("f64::from_bits({expr})"),
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
/// Render bytes as a comma-separated list of `u8` literals (a Rust array body).
///
/// The list is broken onto a new line every `PER_LINE` bytes: a large data
/// segment would otherwise render as one multi-megabyte line that overflows
/// rustc's parser. The embedded newlines sit inside the `[ ... ]` wrapper at the
/// call site, producing an ordinary multi-line array literal.
fn byte_array_literal(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    const PER_LINE: usize = 32;
    let mut out = String::new();
    for (i, b) in bytes.iter().enumerate() {
        out.push_str(if i % PER_LINE == 0 { "\n" } else { " " });
        let _ = write!(out, "{b}u8,");
    }
    out
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
