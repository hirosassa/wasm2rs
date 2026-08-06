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

use std::collections::{HashMap, HashSet};

use wasmparser::{FunctionBody, MemArg, Operator, ValType};

use crate::TranspileError;

mod const_expr;
mod func;
mod helpers;
mod info;
mod render;
mod runtime;
mod simd_rt;
mod wasi;

use self::func::FuncGen;
use self::render::{chunk_prelude, render_lib_root, render_module};
use self::runtime::{render_rt_helpers, rt_name};
use self::simd_rt::render_simd_helpers;

pub(crate) use self::const_expr::{const_expr_to_rust, const_expr_u32};
pub(crate) use self::info::{
    DataSegment, ElemSegment, FuncInput, GlobalInfo, ImportInfo, ImportedGlobalInfo, MemInfo,
    TableInfo, TagInfo, TypeSig,
};
pub(crate) use self::wasi::WasiFn;

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
struct GenMeta {
    /// Instance-method memory helpers, each tagged with the linear memory index
    /// it acts on (0 for the historic single-memory helpers, `i` for `_m{i}`).
    helpers: HashSet<(Helper, u32)>,
    /// Module-scope free-function runtime helpers.
    rt: HashSet<Rt>,
    /// Module-scope lane-wise SIMD helpers, tracked by function name.
    simd: HashSet<&'static str>,
    /// `call_indirect` type indices needing a `call_ref_t{ti}` dispatch method.
    dispatch_sigs: HashSet<u32>,
    /// Whether the function uses legacy exception handling, so the module needs
    /// the [`EXC_TYPE`] definition.
    uses_eh: bool,
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
    /// One `(variable, type)` per result the region yields, in source order.
    results: Vec<(String, ValType)>,
    /// The region's entry parameters (stable operands left on the stack), kept
    /// so an `if`'s `else` arm can restore them after the `then` arm consumes
    /// them.
    entry_params: Vec<Val>,
    /// For a `loop`: one `(variable, type)` per parameter. A `br` back to the
    /// header reassigns these loop-carried variables before `continue`. Empty
    /// for blocks and `if`s.
    loop_params: Vec<(String, ValType)>,
    /// Operand-stack height of the enclosing scope (values below this frame).
    parent_height: usize,
    /// The output buffer of the enclosing scope, restored when the frame ends.
    parent_buffer: Vec<Node>,
    /// For `if`: the `then` branch nodes, captured when `else` is reached.
    then_buffer: Option<Vec<Node>>,
    /// For `if`: whether the `then` branch could fall through to `else`.
    then_reachable: bool,
    /// For `if`: the Rust condition expression.
    cond: Option<String>,
    /// For a legacy-exception `try`: the accumulating handler state. A try frame
    /// reuses [`FrameKind::Block`] (a branch to it behaves like a block exit) but
    /// carries this so `end` renders it as a `catch_unwind` region instead.
    try_state: Option<TryState>,
}

/// The kind of one arm of a `try` region.
#[derive(Clone, PartialEq, Eq)]
enum CatchKind {
    /// The protected body.
    Body,
    /// A `catch $tag` handler.
    Tag(u32),
    /// A `catch_all` handler.
    All,
}

/// One finished arm of a `try` region: the body or a catch handler.
struct CatchArm {
    kind: CatchKind,
    /// Prebuilt `let` statements that extract the exception payload into the
    /// handler's operand variables, run before the handler body. Empty for the
    /// body and `catch_all`.
    binds: Vec<String>,
    body: Vec<Node>,
    /// Whether control can fall through this arm's end (so it needs a trailing
    /// `break 'lN;` when the try is targeted).
    reachable_at_end: bool,
}

/// The state accumulated while translating a `try` region, between its opening
/// and its `end`.
struct TryState {
    /// The variable the caught exception box is bound to in the `Err` arm.
    exc_var: String,
    /// Finished arms, in order: the body first, then each catch handler.
    arms: Vec<CatchArm>,
    /// The kind of the arm currently being emitted into `self.cur`.
    cur_kind: CatchKind,
    /// The exception-payload extraction statements of the currently-open catch
    /// handler.
    cur_binds: Vec<String>,
    /// Distinct branch targets that escape this try's body, in discovery order.
    /// The try's outcome variable holds `index + 1` for the branch that fired,
    /// so the post-`match` dispatch can re-issue it outside the closure.
    escapes: Vec<BranchEscape>,
    /// Whether a `return` escapes this try's body (routed through the
    /// function-wide return signal rather than the outcome variable).
    has_ret_escape: bool,
}

/// A branch that leaves a `try` body, recorded so the try's post-`match`
/// dispatch can re-issue it outside the `catch_unwind` closure (as a direct
/// `break`/`continue`, or — when it also escapes an enclosing try — as another
/// closure-outcome signal).
struct BranchEscape {
    /// The frame the branch targets (below the try that it escapes).
    target_idx: usize,
    is_loop: bool,
    label: usize,
}

impl Frame {
    /// The result variable names, in source order.
    fn result_vars(&self) -> Vec<String> {
        self.results.iter().map(|(var, _)| var.clone()).collect()
    }

    /// The loop-carried parameter variable names, in source order.
    fn loop_param_vars(&self) -> Vec<String> {
        self.loop_params
            .iter()
            .map(|(var, _)| var.clone())
            .collect()
    }
}

/// One element of a function body, captured structurally so a whole function can
/// be rendered lazily at `finish` time as either nested Rust (the default) or a
/// flat dispatch loop. Rendering is deferred because the choice depends on the
/// function's peak control-flow nesting, which is only known once the whole body
/// has been generated.
enum Node {
    /// An opaque straight-line statement, already formatted at its region's own
    /// base indentation (region nesting is applied when rendered).
    Line(String),
    /// A statement after which control does not fall through (`return`, a
    /// trapping `panic!`).
    Term(String),
    /// An unconditional branch: `break`/`continue` to region `label`. Any
    /// value-carrying assignments precede it as `Line`s.
    Br { label: usize, is_loop: bool },
    /// A conditional branch (`br_if`): when `cond` is non-zero, run `assigns`
    /// then `break`/`continue` to `label`; otherwise fall through.
    BrIf {
        cond: String,
        label: usize,
        is_loop: bool,
        assigns: Vec<(String, String)>,
    },
    /// A `br_table`: dispatch on `selector` to one of several branch arms.
    BrTable { selector: String, arms: Vec<BrArm> },
    /// A nested control-flow region (block/loop/if).
    Region(RegionNode),
    /// A legacy-exception `try` region, rendered as a `catch_unwind` over the
    /// protected body with a landing pad dispatching on the caught tag.
    Try(TryRegionNode),
}

/// A finished `try` region: the protected body plus its catch handlers.
struct TryRegionNode {
    /// Numeric label; the body is wrapped in `'lN: loop { … }` when `targeted`
    /// so a `br` to the try becomes a `break` out of the protected body.
    label: usize,
    targeted: bool,
    /// The variable the caught exception box binds to in the landing pad.
    exc_var: String,
    /// The protected body.
    body: Vec<Node>,
    /// Whether the body can fall through its end (needs a trailing `break`).
    body_reachable_at_end: bool,
    /// The catch handlers, in source order.
    catches: Vec<CatchArm>,
}

/// One arm of a [`Node::BrTable`]: a match pattern that assigns its target's
/// value-carrying variables then `break`/`continue`s to `label`.
struct BrArm {
    pattern: String,
    label: usize,
    is_loop: bool,
    assigns: Vec<(String, String)>,
}

impl BrArm {
    fn keyword(&self) -> &'static str {
        if self.is_loop { "continue" } else { "break" }
    }
}

/// A finished control-flow region, retained structurally (rather than eagerly
/// flattened to indented text) so it can be rendered nested or flattened.
struct RegionNode {
    kind: FrameKind,
    /// Numeric label; only rendered as `'lN` when `targeted`.
    label: usize,
    /// Whether some `br`/`br_if`/`br_table` targets this region.
    targeted: bool,
    /// Whether control could fall through the region's end (so a targeted
    /// block/loop needs a trailing `break 'lN;`).
    reachable_at_end: bool,
    /// For an `if`: its Rust condition expression; `None` for block/loop.
    cond: Option<String>,
    /// For block/loop: the whole body. For `if`: the `then` arm.
    body: Vec<Node>,
    /// For an `if` with an explicit or implicit `else`: the `else` arm.
    els: Option<Vec<Node>>,
}

/// Render a function body (its deferred [`Node`] list) into `out`, consuming it.
/// Each non-empty line is written as `line_prefix` + the four-space function-body
/// indent + four spaces per control-nesting level + the statement; blank lines
/// stay bare. This reproduces, byte for byte, the previously eager
/// `indent`-based rendering, while consuming and dropping each node as it is
/// copied so a huge body is never held twice.
fn render_body_into(nodes: Vec<Node>, line_prefix: &str, out: &mut String) {
    render_nodes_into(nodes, 0, line_prefix, out);
}

fn render_nodes_into(nodes: Vec<Node>, depth: usize, line_prefix: &str, out: &mut String) {
    for node in nodes {
        match node {
            Node::Line(text) | Node::Term(text) => push_body_line(out, &text, depth, line_prefix),
            Node::Br { label, is_loop } => {
                let keyword = if is_loop { "continue" } else { "break" };
                push_body_line(out, &format!("{keyword} 'l{label};"), depth, line_prefix);
            }
            Node::BrIf {
                cond,
                label,
                is_loop,
                assigns,
            } => render_br_if_nested(&cond, label, is_loop, &assigns, depth, line_prefix, out),
            Node::BrTable { selector, arms } => {
                render_br_table_nested(&selector, &arms, depth, line_prefix, out)
            }
            Node::Region(region) => render_region_into(region, depth, line_prefix, out),
            Node::Try(try_node) => render_try_into(try_node, depth, line_prefix, out),
        }
    }
}

/// Render a wasm i32 condition value as a Rust `bool`. A comparison is emitted
/// as `i32::from(<cmp>)`, so a branch condition's `i32::from(<cmp>) != 0`
/// collapses to just `<cmp>`; any other value keeps an explicit `!= 0`.
pub(crate) fn condition_code(code: &str) -> String {
    match bool_inner(code) {
        Some(inner) => inner.to_string(),
        None => format!("{code} != 0"),
    }
}

/// If `code` is exactly `i32::from(<balanced>)` — the shape a comparison emits —
/// return the inner boolean expression; otherwise `None`. The balance check
/// ensures the stripped parentheses are the outermost pair, so a compound like
/// `(i32::from(a) & i32::from(b))` (which does not start with `i32::from(`) or a
/// truncated match is never unwrapped.
fn bool_inner(code: &str) -> Option<&str> {
    let inner = code.strip_prefix("i32::from(")?.strip_suffix(')')?;
    let mut depth: i32 = 0;
    for b in inner.bytes() {
        match b {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth < 0 {
                    return None;
                }
            }
            _ => {}
        }
    }
    if depth == 0 { Some(inner) } else { None }
}

/// Render a `br_if` node as nested Rust, matching the byte layout the eager
/// renderer produced: a one-line `if` when it carries no values, or a
/// multi-line `if` whose body assigns the carried values (with the historic
/// four-space inner indent baked into the statement) before branching.
fn render_br_if_nested(
    cond: &str,
    label: usize,
    is_loop: bool,
    assigns: &[(String, String)],
    depth: usize,
    line_prefix: &str,
    out: &mut String,
) {
    let keyword = if is_loop { "continue" } else { "break" };
    if assigns.is_empty() {
        push_body_line(
            out,
            &format!("if {cond} {{ {keyword} 'l{label}; }}"),
            depth,
            line_prefix,
        );
    } else {
        push_body_line(out, &format!("if {cond} {{"), depth, line_prefix);
        for (var, value) in assigns {
            push_body_line(out, &format!("    {var} = {value};"), depth, line_prefix);
        }
        push_body_line(
            out,
            &format!("    {keyword} 'l{label};"),
            depth,
            line_prefix,
        );
        push_body_line(out, "}", depth, line_prefix);
    }
}

/// Render a `br_table` node as nested Rust (`match (sel) as u32 { … }`),
/// matching the byte layout the eager renderer produced.
fn render_br_table_nested(
    selector: &str,
    arms: &[BrArm],
    depth: usize,
    line_prefix: &str,
    out: &mut String,
) {
    push_body_line(
        out,
        &format!("match ({selector}) as u32 {{"),
        depth,
        line_prefix,
    );
    for arm in arms {
        let keyword = arm.keyword();
        let label = arm.label;
        let line = if arm.assigns.is_empty() {
            format!("    {} => {keyword} 'l{label},", arm.pattern)
        } else {
            let assigns: String = arm
                .assigns
                .iter()
                .map(|(var, value)| format!("{var} = {value}; "))
                .collect();
            format!(
                "    {} => {{ {assigns}{keyword} 'l{label}; }},",
                arm.pattern
            )
        };
        push_body_line(out, &line, depth, line_prefix);
    }
    push_body_line(out, "}", depth, line_prefix);
}

/// Write one body line at control-nesting `depth`. Empty lines stay bare
/// (matching the single-file renderer's `indent`, which leaves them empty).
fn push_body_line(out: &mut String, text: &str, depth: usize, line_prefix: &str) {
    if !text.is_empty() {
        out.push_str(line_prefix);
        // Base function-body indent, then one level per enclosing region.
        for _ in 0..=depth {
            out.push_str("    ");
        }
        out.push_str(text);
    }
    out.push('\n');
}

fn render_region_into(region: RegionNode, depth: usize, line_prefix: &str, out: &mut String) {
    let RegionNode {
        kind,
        label,
        targeted,
        reachable_at_end,
        cond,
        body,
        els,
    } = region;

    // The inner content (block/loop body, or the `if { … } else { … }`),
    // rendered at `inner_depth`; a targeted region wraps it in a labelled loop.
    let inner_depth = if targeted { depth + 1 } else { depth };
    let render_inner = |out: &mut String, body: Vec<Node>, els: Option<Vec<Node>>| match kind {
        FrameKind::Block | FrameKind::Loop => {
            render_nodes_into(body, inner_depth, line_prefix, out)
        }
        FrameKind::If => {
            let cond = cond.clone().unwrap_or_default();
            push_body_line(out, &format!("if {cond} {{"), inner_depth, line_prefix);
            render_nodes_into(body, inner_depth + 1, line_prefix, out);
            if let Some(els) = els {
                push_body_line(out, "} else {", inner_depth, line_prefix);
                render_nodes_into(els, inner_depth + 1, line_prefix, out);
            }
            push_body_line(out, "}", inner_depth, line_prefix);
        }
    };

    if targeted {
        // A `loop` has a back-edge (`br` to it `continue`s), so it stays a
        // labelled `loop` and needs a trailing `break` to leave it when the body
        // falls through. A `block`/`if` never loops, so it is a labelled block
        // `'lN: { … }` that a `br` `break`s out of and that falls through its end
        // naturally — no trailing break.
        let is_loop = kind == FrameKind::Loop;
        let header = if is_loop {
            format!("'l{label}: loop {{")
        } else {
            format!("'l{label}: {{")
        };
        push_body_line(out, &header, depth, line_prefix);
        render_inner(out, body, els);
        if is_loop && reachable_at_end {
            push_body_line(out, &format!("break 'l{label};"), inner_depth, line_prefix);
        }
        push_body_line(out, "}", depth, line_prefix);
    } else {
        render_inner(out, body, els);
    }
}

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

/// Render a `try` region as a `catch_unwind` over the protected body followed by
/// a landing pad that dispatches on the caught exception's tag. A thrown wasm
/// exception is a `panic_any` of [`EXC_TYPE`]; any other payload (a trap, or a
/// foreign panic) is re-raised so only wasm exceptions are caught.
fn render_try_into(node: TryRegionNode, depth: usize, line_prefix: &str, out: &mut String) {
    let TryRegionNode {
        label,
        targeted,
        exc_var,
        body,
        body_reachable_at_end,
        catches,
    } = node;

    push_body_line(
        out,
        "match ::std::panic::catch_unwind(::std::panic::AssertUnwindSafe(|| {",
        depth,
        line_prefix,
    );
    // The protected body. A targeted try wraps it in a labelled loop so a `br`
    // to the try is a `break` that leaves the body (returning `()` normally).
    if targeted {
        push_body_line(out, &format!("'l{label}: loop {{"), depth + 1, line_prefix);
        render_nodes_into(body, depth + 2, line_prefix, out);
        if body_reachable_at_end {
            push_body_line(out, &format!("break 'l{label};"), depth + 2, line_prefix);
        }
        push_body_line(out, "}", depth + 1, line_prefix);
    } else {
        render_nodes_into(body, depth + 1, line_prefix, out);
    }
    push_body_line(out, "})) {", depth, line_prefix);
    push_body_line(out, "Ok(()) => {}", depth + 1, line_prefix);
    push_body_line(
        out,
        &format!("Err({exc_var}) => {{"),
        depth + 1,
        line_prefix,
    );
    render_landing_pad(catches, &exc_var, depth + 2, line_prefix, out);
    push_body_line(out, "}", depth + 1, line_prefix);
    push_body_line(out, "}", depth, line_prefix);
}

/// Render the `Err` arm of a try's `catch_unwind`: resolve the caught tag and
/// dispatch to the matching handler, re-raising anything unmatched. Consumes the
/// handler nodes (like [`render_nodes_into`]) so a huge body is never held twice.
fn render_landing_pad(
    catches: Vec<CatchArm>,
    exc_var: &str,
    depth: usize,
    line_prefix: &str,
    out: &mut String,
) {
    if catches.is_empty() {
        // Nothing this try catches, so always re-raise.
        push_body_line(
            out,
            &format!("::std::panic::resume_unwind({exc_var});"),
            depth,
            line_prefix,
        );
        return;
    }
    // A `catch_all` present makes the wildcard arm run its handler; otherwise an
    // unmatched wasm exception is re-raised.
    let has_all = catches.iter().any(|c| c.kind == CatchKind::All);
    push_body_line(
        out,
        &format!("let __tag = match {exc_var}.downcast_ref::<{EXC_TYPE}>() {{"),
        depth,
        line_prefix,
    );
    push_body_line(out, "Some(__e) => __e.tag,", depth + 1, line_prefix);
    push_body_line(
        out,
        &format!("None => ::std::panic::resume_unwind({exc_var}),"),
        depth + 1,
        line_prefix,
    );
    push_body_line(out, "};", depth, line_prefix);
    push_body_line(out, "match __tag {", depth, line_prefix);
    for arm in catches {
        let CatchArm {
            kind, binds, body, ..
        } = arm;
        match kind {
            CatchKind::Tag(tag) => {
                push_body_line(out, &format!("{tag}u32 => {{"), depth + 1, line_prefix);
            }
            CatchKind::All => {
                push_body_line(out, "_ => {", depth + 1, line_prefix);
            }
            // The body arm is never part of `catches`.
            CatchKind::Body => continue,
        }
        for bind in binds {
            push_body_line(out, &bind, depth + 2, line_prefix);
        }
        render_nodes_into(body, depth + 2, line_prefix, out);
        push_body_line(out, "}", depth + 1, line_prefix);
    }
    if !has_all {
        push_body_line(
            out,
            &format!("_ => ::std::panic::resume_unwind({exc_var}),"),
            depth + 1,
            line_prefix,
        );
    }
    push_body_line(out, "}", depth, line_prefix);
}

/// The estimated byte length of a rendered body, used to pre-reserve the output
/// buffer so appending a huge function never triggers repeated `String`
/// doublings. Indentation is ignored; the statement text dominates.
fn estimate_body_len(nodes: &[Node]) -> usize {
    nodes
        .iter()
        .map(|node| match node {
            Node::Line(text) | Node::Term(text) => text.len() + 5,
            Node::Br { .. } => 24,
            Node::BrIf { cond, assigns, .. } => {
                cond.len()
                    + assigns
                        .iter()
                        .map(|(v, e)| v.len() + e.len() + 8)
                        .sum::<usize>()
                    + 32
            }
            Node::BrTable { selector, arms } => {
                selector.len()
                    + arms
                        .iter()
                        .map(|a| {
                            a.pattern.len()
                                + a.assigns
                                    .iter()
                                    .map(|(v, e)| v.len() + e.len() + 6)
                                    .sum::<usize>()
                                + 24
                        })
                        .sum::<usize>()
                    + 32
            }
            Node::Region(region) => {
                estimate_body_len(&region.body)
                    + region.els.as_deref().map_or(0, estimate_body_len)
                    + 32
            }
            Node::Try(t) => {
                estimate_body_len(&t.body)
                    + t.catches
                        .iter()
                        .map(|c| {
                            estimate_body_len(&c.body)
                                + c.binds.iter().map(|b| b.len() + 5).sum::<usize>()
                                + 32
                        })
                        .sum::<usize>()
                    // The `catch_unwind`/landing-pad scaffolding.
                    + 160
            }
        })
        .sum()
}

/// Peak control-flow nesting above which a function is emitted as a flat
/// dispatch loop instead of nested Rust, so its rendered nesting cannot overflow
/// rustc's recursive-descent parser. Chosen well above any hand-written nesting
/// (so ordinary functions keep their readable nested form) yet far below the
/// parser's stack limit.
const FLATTEN_DEPTH_THRESHOLD: usize = 40;

/// Whether a body can be lowered to a flat dispatch loop with the currently
/// supported constructs (block/loop/`br`/`br_if`/terminators). `if` regions and
/// `br_table` are not yet flattenable, so a body containing them stays nested.
///
/// Walked with an explicit stack rather than by recursion, so a deeply nested
/// module (the very case that triggers flattening) cannot overflow the thread's
/// stack while deciding whether to flatten.
fn can_flatten(nodes: &[Node]) -> bool {
    let mut stack: Vec<&[Node]> = vec![nodes];
    while let Some(nodes) = stack.pop() {
        for node in nodes {
            // A `try` region relies on Rust's `catch_unwind` scaffolding, which
            // has no flat-dispatch lowering, so a body containing one stays
            // nested.
            if matches!(node, Node::Try(_)) {
                return false;
            }
            let Node::Region(region) = node else { continue };
            // A `br_if`-only `if` (no condition) has no flat lowering.
            if region.kind == FrameKind::If && region.cond.is_none() {
                return false;
            }
            stack.push(&region.body);
            if let Some(els) = region.els.as_deref() {
                stack.push(els);
            }
        }
    }
    true
}

/// Depth budget for structuring a loop: a back-edge loop whose body nests no
/// deeper than this is emitted as a real `'lN: loop { … }` (with its nested
/// `if`/`block`/`loop` also structured) instead of state-machine arms. A
/// structured loop sits on the flat dispatch's shallow base, so its rendered
/// nesting is at most a small constant plus this budget — kept comfortably under
/// the parser limit that [`FLATTEN_DEPTH_THRESHOLD`] guards. Loops deeper than
/// this stay on the generic flat path. (Measured on real modules, back-edge
/// loops nest ≤45, so this captures nearly all of them.)
const STRUCT_BUDGET: usize = 32;

/// Whether the flattener can emit `region` as a structured `'lN: loop { … }`
/// rather than dispatch arms: a `loop` that is actually branched back to
/// (`targeted`), whose body nests within [`STRUCT_BUDGET`], and whose subtree
/// the structured renderer can emit (no `try`, no condition-less `if` — the same
/// constructs [`can_flatten`] rejects). Requiring a real back-edge guarantees
/// the structured loop's `'lN` label and `continue 'lN` are used (an unused
/// label would fail `-D warnings`) and keeps the specialisation to loops that
/// actually repeat.
fn is_structurable_loop(region: &RegionNode) -> bool {
    region.kind == FrameKind::Loop
        && region.targeted
        && subtree_depth(&region.body) <= STRUCT_BUDGET
        && can_flatten(&region.body)
}

/// Lower a structured body to a flat state machine: `let mut pc = …; 'sm: loop {
/// match pc { … } }`. Each control region becomes one or more flat `match` arms
/// linked by `pc` assignments, so the rendered nesting is a small constant
/// regardless of the wasm nesting depth. The caller must have checked
/// [`can_flatten`] first.
///
/// `trailing` is the function's tail expression (the value produced by falling
/// through the whole body), returned from the exit state. Every arm either loops
/// (setting `pc`) or `return`s, so the dispatch loop diverges (`!`) and is a
/// valid tail for any return type. `returns_value` says whether the function has
/// a result: a value-returning function with no `trailing` never falls through,
/// so its exit state is unreachable. Returns the flat body as already-indented
/// [`Node::Line`]s.
/// Max region-nesting depth within `nodes` (0 if nothing nested). Bounds how
/// deep a structured loop would render; [`is_structurable_loop`] uses it to keep
/// the specialised nesting well under rustc's parser limit.
fn subtree_depth(nodes: &[Node]) -> usize {
    let mut max = 0;
    for node in nodes {
        let d = match node {
            Node::Region(r) => {
                let els = r.els.as_deref().map_or(0, subtree_depth);
                1 + subtree_depth(&r.body).max(els)
            }
            Node::Try(t) => 1 + subtree_depth(&t.body),
            _ => 0,
        };
        max = max.max(d);
    }
    max
}

fn flatten_body(nodes: Vec<Node>, trailing: Option<String>, returns_value: bool) -> Vec<Node> {
    let mut f = Flattener {
        arms: Vec::new(),
        next_state: 0,
        labels: HashMap::new(),
        uses_continue: false,
    };
    let start = f.alloc();
    let exit = f.alloc();
    let exit_stmt = match trailing {
        Some(expr) => format!("return {expr};"),
        // A value-returning function reaches its exit only by falling through
        // with a value (a `Some` trailing); reaching it otherwise is impossible.
        None if returns_value => "unreachable!();".to_string(),
        None => "return;".to_string(),
    };
    f.arms.push((exit, vec![(0, exit_stmt)]));
    f.lower(nodes, start, exit);
    f.assemble(start)
}

/// One statement of a dispatch arm: its indent *relative to the arm body* (0 for
/// an ordinary flat statement; >0 for lines nested inside a structured loop) and
/// the statement text. [`Flattener::assemble`] adds the arm's base indent.
type ArmLine = (usize, String);

/// Builds the flat `match pc { … }` arms while linearising a structured body.
struct Flattener {
    /// Completed dispatch arms: `(state id, its statements)`.
    arms: Vec<(usize, Vec<ArmLine>)>,
    /// Next unused state id.
    next_state: usize,
    /// wasm region label → the state a branch to it jumps to (a loop's header,
    /// a block's continuation).
    labels: HashMap<usize, usize>,
    /// Whether any arm uses `continue 'sm` (emitted for `br_if`), so the
    /// dispatch loop needs its `'sm` label. Without it the label is unused.
    uses_continue: bool,
}

impl Flattener {
    fn alloc(&mut self) -> usize {
        let id = self.next_state;
        self.next_state += 1;
        id
    }

    /// Linearise `nodes` so execution enters at state `entry` and, on
    /// fall-through, transfers to state `after`.
    ///
    /// A nested region does not recurse: its body is pushed onto a worklist as a
    /// `(nodes, entry, after)` task and lowered in a later iteration, so the
    /// nesting depth becomes worklist length rather than call-stack depth and a
    /// deeply nested module cannot overflow the stack. Order across tasks is
    /// irrelevant because arms are collected into `self.arms` and sorted by state
    /// id in [`Self::assemble`]; a region's label is inserted into `self.labels`
    /// before its body task is pushed, and a branch only ever targets an
    /// enclosing region (never a sibling or descendant), so every label a task
    /// reads is already present.
    fn lower(&mut self, nodes: Vec<Node>, entry: usize, after: usize) {
        let mut worklist: Vec<(Vec<Node>, usize, usize)> = vec![(nodes, entry, after)];
        while let Some((nodes, entry, after)) = worklist.pop() {
            let mut state = entry;
            let mut stmts: Vec<ArmLine> = Vec::new();
            let mut reachable = true;
            for node in nodes {
                if !reachable {
                    // The generator does not emit past a terminator within a
                    // region, but skip defensively so a stray node cannot start a
                    // dead arm.
                    continue;
                }
                match node {
                    Node::Line(text) => stmts.push((0, text)),
                    Node::Term(text) => {
                        stmts.push((0, text));
                        self.arms.push((state, std::mem::take(&mut stmts)));
                        reachable = false;
                    }
                    Node::Br { label, .. } => {
                        let target = self.labels[&label];
                        stmts.push((0, format!("pc = {target};")));
                        self.arms.push((state, std::mem::take(&mut stmts)));
                        reachable = false;
                    }
                    Node::BrIf {
                        cond,
                        label,
                        assigns,
                        ..
                    } => {
                        let target = self.labels[&label];
                        let mut line = format!("if {cond} {{ ");
                        for (var, value) in assigns {
                            line.push_str(&format!("{var} = {value}; "));
                        }
                        line.push_str(&format!("pc = {target}; continue 'sm; }}"));
                        self.uses_continue = true;
                        stmts.push((0, line));
                    }
                    Node::Region(region) if region.kind == FrameKind::If => {
                        // Dispatch to the `then`/`else` entry, then rejoin at
                        // `cont`. A branch to the `if` exits to `cont`.
                        let cont = self.alloc();
                        let then_e = self.alloc();
                        self.labels.insert(region.label, cont);
                        let cond = region.cond.clone().unwrap_or_default();
                        let RegionNode { body, els, .. } = region;
                        let else_e = match &els {
                            Some(_) => self.alloc(),
                            None => cont,
                        };
                        stmts.push((
                            0,
                            format!("if {cond} {{ pc = {then_e}; }} else {{ pc = {else_e}; }}"),
                        ));
                        self.arms.push((state, std::mem::take(&mut stmts)));
                        worklist.push((body, then_e, cont));
                        if let Some(els) = els {
                            worklist.push((els, else_e, cont));
                        }
                        state = cont;
                    }
                    Node::Region(region) if is_structurable_loop(&region) => {
                        // A back-edge loop shallow enough to structure: emit a real
                        // `'lN: loop { … }` (its nested `if`/`block`/`loop` also
                        // structured) in one arm, so the hot back-edge is a direct
                        // `continue 'lN` and per-iteration branches are direct,
                        // instead of `pc = …; continue 'sm` back through the
                        // jump-table dispatch (which profiling showed dominates the
                        // real parser's cost). The enclosing block nest still
                        // flattens, so rendered nesting grows only by the loop's own
                        // bounded depth. The loop's label stays out of `self.labels`
                        // so a branch to it resolves structurally, not to a state.
                        let header = self.alloc();
                        let cont = self.alloc();
                        stmts.push((0, format!("pc = {header};")));
                        self.arms.push((state, std::mem::take(&mut stmts)));
                        let loop_arm = self.render_structured_loop(region, cont);
                        self.arms.push((header, loop_arm));
                        state = cont;
                    }
                    Node::Region(region) => {
                        let inner = self.alloc();
                        let cont = self.alloc();
                        // A branch to a loop resumes its header; to a block it
                        // exits to the continuation.
                        let branch_target = match region.kind {
                            FrameKind::Loop => inner,
                            _ => cont,
                        };
                        self.labels.insert(region.label, branch_target);
                        stmts.push((0, format!("pc = {inner};")));
                        self.arms.push((state, std::mem::take(&mut stmts)));
                        worklist.push((region.body, inner, cont));
                        state = cont;
                    }
                    Node::Try(t) => {
                        // A `try` keeps [`can_flatten`] from choosing this path,
                        // so this is unreachable in practice; render it to text
                        // and keep it as one opaque statement so a stray one is
                        // still emitted correctly rather than dropped.
                        let mut buf = String::new();
                        render_try_into(t, 0, "", &mut buf);
                        stmts.push((0, buf.trim_end().to_string()));
                    }
                    Node::BrTable { selector, arms } => {
                        // Each arm assigns its target's carried variables then
                        // sets `pc`; the whole `match` ends the current arm.
                        let mut line = format!("match ({selector}) as u32 {{ ");
                        for arm in arms {
                            let target = self.labels[&arm.label];
                            line.push_str(&format!("{} => {{ ", arm.pattern));
                            for (var, value) in arm.assigns {
                                line.push_str(&format!("{var} = {value}; "));
                            }
                            line.push_str(&format!("pc = {target}; }} "));
                        }
                        line.push('}');
                        stmts.push((0, line));
                        self.arms.push((state, std::mem::take(&mut stmts)));
                        reachable = false;
                    }
                }
            }
            if reachable {
                stmts.push((0, format!("pc = {after};")));
                self.arms.push((state, stmts));
            }
        }
    }

    /// Render a structurable loop (checked by [`is_structurable_loop`]) as a real
    /// `'l{label}: loop { … }` occupying one dispatch arm. The body — including
    /// any nested `if`/`block`/`loop` — is emitted as structured Rust, so
    /// back-edges and in-loop branches are direct `break`/`continue`s. A branch
    /// that leaves the whole structured subtree (its target is an enclosing
    /// flattened region, hence a state in `self.labels`) becomes `pc = <state>;
    /// continue 'sm`, exiting both the loop and the dispatch. A natural
    /// fall-through off the loop's end `break`s and resumes the dispatch at `cont`.
    ///
    /// Lines carry an indent relative to the arm body; [`Self::assemble`] adds the
    /// arm's base indent.
    fn render_structured_loop(&mut self, region: RegionNode, cont: usize) -> Vec<ArmLine> {
        let mut out = Vec::new();
        let reachable_at_end = region.reachable_at_end;
        // The loop and its subtree render structurally; only the trailing
        // dispatch resumption (`pc = cont`) links back to the flat machine.
        self.render_structured_region(region, 0, &mut out);
        // A loop that can fall through its end exits to `cont`; one that only ever
        // back-edges or diverges has no `break`, so the `loop` is `!` and nothing
        // may follow it (an unreachable `pc = cont` would fail `-D warnings`).
        if reachable_at_end {
            out.push((0, format!("pc = {cont};")));
        }
        out
    }

    /// Render a structured node list at `depth` (indent relative to the arm
    /// body). Branch targets inside this subtree resolve to structured
    /// `break`/`continue 'lN`; a target that is a flattened state (present in
    /// `self.labels`) becomes `pc = <state>; continue 'sm`.
    fn render_structured(&mut self, nodes: Vec<Node>, depth: usize, out: &mut Vec<ArmLine>) {
        for node in nodes {
            match node {
                Node::Line(text) | Node::Term(text) => out.push((depth, text)),
                Node::Br { label, is_loop } => {
                    out.push((depth, self.structured_branch(label, is_loop, "")));
                }
                Node::BrIf {
                    cond,
                    label,
                    is_loop,
                    assigns,
                } => {
                    if assigns.is_empty() {
                        let br = self.structured_branch(label, is_loop, "");
                        out.push((depth, format!("if {cond} {{ {br} }}")));
                    } else {
                        out.push((depth, format!("if {cond} {{")));
                        for (var, value) in assigns {
                            out.push((depth + 1, format!("{var} = {value};")));
                        }
                        let br = self.structured_branch(label, is_loop, "");
                        out.push((depth + 1, br));
                        out.push((depth, "}".to_string()));
                    }
                }
                Node::BrTable { selector, arms } => {
                    out.push((depth, format!("match ({selector}) as u32 {{")));
                    for arm in arms {
                        let assigns: String = arm
                            .assigns
                            .iter()
                            .map(|(var, value)| format!("{var} = {value}; "))
                            .collect();
                        let br = self.structured_branch(arm.label, arm.is_loop, &assigns);
                        out.push((depth + 1, format!("{} => {{ {br} }},", arm.pattern)));
                    }
                    out.push((depth, "}".to_string()));
                }
                Node::Region(region) => self.render_structured_region(region, depth, out),
                Node::Try(_) => unreachable!("is_structurable_loop rejects `try` subtrees"),
            }
        }
    }

    /// A structured branch to `label`: `continue`/`break 'l{label}` when the
    /// target is a region in this subtree, or `pc = <state>; continue 'sm` when it
    /// is an enclosing flattened region (a state in `self.labels`). `assigns` is a
    /// prebuilt run of `var = value; ` statements that must precede the branch.
    fn structured_branch(&mut self, label: usize, is_loop: bool, assigns: &str) -> String {
        if let Some(&state) = self.labels.get(&label) {
            self.uses_continue = true;
            format!("{assigns}pc = {state}; continue 'sm;")
        } else {
            let keyword = if is_loop { "continue" } else { "break" };
            format!("{assigns}{keyword} 'l{label};")
        }
    }

    /// Render one region as structured Rust at `depth`, mirroring
    /// [`render_region_into`] but emitting arm lines and routing escaping branches
    /// through [`Self::structured_branch`].
    fn render_structured_region(
        &mut self,
        region: RegionNode,
        depth: usize,
        out: &mut Vec<ArmLine>,
    ) {
        let RegionNode {
            kind,
            label,
            targeted,
            reachable_at_end,
            cond,
            body,
            els,
        } = region;
        // A targeted region is wrapped in a label so a branch to it can
        // `break`/`continue` here; its content renders one level deeper.
        let inner_depth = if targeted { depth + 1 } else { depth };
        if targeted {
            let header = if kind == FrameKind::Loop {
                format!("'l{label}: loop {{")
            } else {
                format!("'l{label}: {{")
            };
            out.push((depth, header));
        }
        match kind {
            FrameKind::Block | FrameKind::Loop => self.render_structured(body, inner_depth, out),
            FrameKind::If => {
                let cond = cond.unwrap_or_default();
                out.push((inner_depth, format!("if {cond} {{")));
                self.render_structured(body, inner_depth + 1, out);
                if let Some(els) = els {
                    out.push((inner_depth, "} else {".to_string()));
                    self.render_structured(els, inner_depth + 1, out);
                }
                out.push((inner_depth, "}".to_string()));
            }
        }
        if targeted {
            // A `loop` falling through its end needs a trailing `break` to leave
            // it; a `block`/`if` falls through its labelled scope naturally.
            if kind == FrameKind::Loop && reachable_at_end {
                out.push((inner_depth, format!("break 'l{label};")));
            }
            out.push((depth, "}".to_string()));
        }
    }

    /// Assemble the dispatch loop from the collected arms, indenting each line to
    /// its final position (relative to the function body's first level).
    ///
    /// Each `match` arm is its own lexical scope, so a `let` binding declared in
    /// one arm would be invisible to the others. Every typed `let` is therefore
    /// hoisted to a declaration above the loop (kept in scope for all arms) while
    /// its initialising assignment — which may carry side effects and must run at
    /// its original program point — stays in the arm.
    fn assemble(mut self, start: usize) -> Vec<Node> {
        let start = contract_pc_edges(&mut self.arms, start);
        self.arms.sort_by_key(|(state, _)| *state);

        let mut decls: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut rewritten: Vec<(usize, Vec<ArmLine>)> = Vec::with_capacity(self.arms.len());
        for (state, stmts) in self.arms {
            let mut arm = Vec::with_capacity(stmts.len());
            for (indent, stmt) in stmts {
                match hoist_decl(&stmt) {
                    Some((decl, assign)) => {
                        if seen.insert(decl.clone()) {
                            decls.push(decl);
                        }
                        arm.push((indent, assign));
                    }
                    None => arm.push((indent, stmt)),
                }
            }
            rewritten.push((state, arm));
        }

        let mut out = Vec::new();
        let mut push = |depth: usize, text: String| {
            out.push(Node::Line(format!("{}{text}", "    ".repeat(depth))));
        };
        push(0, format!("let mut pc: usize = {start};"));
        for decl in decls {
            push(0, decl);
        }
        // The `'sm` label is only needed when an arm uses `continue 'sm`.
        let loop_head = if self.uses_continue {
            "'sm: loop {"
        } else {
            "loop {"
        };
        push(0, loop_head.to_string());
        push(1, "match pc {".to_string());
        for (state, stmts) in rewritten {
            push(2, format!("{state} => {{"));
            // A structured-loop arm nests lines inside `'lN: loop { … }`; each line
            // carries its indent relative to the arm body (0 for ordinary flat
            // statements).
            for (indent, stmt) in stmts {
                push(3 + indent, stmt);
            }
            push(2, "}".to_string());
        }
        push(2, "_ => unreachable!(),".to_string());
        push(1, "}".to_string());
        push(0, "}".to_string());
        out
    }
}

/// If `body` is a single unconditional `pc = N;` (a pure forwarding state with
/// no side effect), return `N`. Such a state only re-enters the `match pc`
/// dispatch to jump again, so every edge to it can skip straight to `N`.
fn trivial_pc_target(body: &[ArmLine]) -> Option<usize> {
    let [(_, line)] = body else { return None };
    line.strip_prefix("pc = ")?.strip_suffix(';')?.parse().ok()
}

/// Rewrite every `pc = <state>` target in `s` through `map` (leaving all other
/// text untouched). `pc = ` and the digits are ASCII, so verbatim spans are
/// copied at valid UTF-8 boundaries.
fn rewrite_pc_targets(s: &str, map: &impl Fn(usize) -> usize) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut copied = 0;
    let mut i = 0;
    while i + 5 <= b.len() {
        if &b[i..i + 5] == b"pc = " {
            let ns = i + 5;
            let mut j = ns;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            if j > ns {
                out.push_str(&s[copied..ns]);
                out.push_str(&map(s[ns..j].parse::<usize>().unwrap()).to_string());
                copied = j;
                i = j;
                continue;
            }
        }
        i += 1;
    }
    out.push_str(&s[copied..]);
    out
}

/// Contract pure `pc = N;` forwarding states out of a flattened dispatch: every
/// edge targeting such a state is redirected to the state it forwards to
/// (following chains), and the states that become unreachable are dropped.
///
/// A deeply nested block/`if` spine lowers to long runs of these pure jumps —
/// descending and unwinding the nest costs one dispatch round-trip per level.
/// Profiling the googlesql parser (its hottest function, ~98% of parse time)
/// showed ~40% of its 7183 arms were exactly these. Folding them removes that
/// share of the indirect-branch traffic through the jump table. Returns the
/// (possibly redirected) start state.
fn contract_pc_edges(arms: &mut Vec<(usize, Vec<ArmLine>)>, start: usize) -> usize {
    // Each pure forwarding state → its single constant successor.
    let fwd: HashMap<usize, usize> = arms
        .iter()
        .filter_map(|(state, body)| Some((*state, trivial_pc_target(body)?)))
        .collect();
    if fwd.is_empty() {
        return start;
    }

    // Resolve every state to the first non-forwarding state on its chain, or —
    // for a forwarding cycle (a side-effect-free infinite loop) — an entry of the
    // cycle, kept alive so the loop is preserved. Memoised with path compression.
    let mut resolved: HashMap<usize, usize> = HashMap::new();
    for &s0 in fwd.keys() {
        if resolved.contains_key(&s0) {
            continue;
        }
        let mut path = Vec::new();
        let mut cur = s0;
        let dest = loop {
            if let Some(&d) = resolved.get(&cur) {
                break d;
            }
            match fwd.get(&cur) {
                Some(&next) if !path.contains(&cur) => {
                    path.push(cur);
                    cur = next;
                }
                // A cycle closes at `cur` (keep it), or `cur` is terminal.
                _ => break cur,
            }
        };
        for p in path {
            resolved.insert(p, dest);
        }
    }
    let resolve = |t: usize| resolved.get(&t).copied().unwrap_or(t);

    // Redirect all targets, in arm bodies and at the entry.
    for (_, body) in arms.iter_mut() {
        for (_, line) in body.iter_mut() {
            *line = rewrite_pc_targets(line, &resolve);
        }
    }
    let start = resolve(start);

    // Drop states no edge reaches any more (the folded forwarders, plus any
    // states that were only reachable through them). Reachability walks the `pc =
    // N` targets in the redirected bodies from the entry.
    let index: HashMap<usize, usize> = arms
        .iter()
        .enumerate()
        .map(|(i, (state, _))| (*state, i))
        .collect();
    let mut live: HashSet<usize> = HashSet::new();
    let mut stack = vec![start];
    while let Some(state) = stack.pop() {
        if !live.insert(state) {
            continue;
        }
        let Some(&i) = index.get(&state) else {
            continue;
        };
        for (_, line) in &arms[i].1 {
            let b = line.as_bytes();
            let mut k = 0;
            while k + 5 <= b.len() {
                if &b[k..k + 5] == b"pc = " {
                    let ns = k + 5;
                    let mut j = ns;
                    while j < b.len() && b[j].is_ascii_digit() {
                        j += 1;
                    }
                    if j > ns {
                        stack.push(line[ns..j].parse().unwrap());
                        k = j;
                        continue;
                    }
                }
                k += 1;
            }
        }
    }
    arms.retain(|(state, _)| live.contains(state));
    start
}

/// If `line` is a typed `let` binding, return `(hoisted declaration, in-arm
/// assignment)`: the declaration is placed above the dispatch loop so it stays
/// in scope for every arm, while the assignment (carrying any side effects)
/// stays at the original program point. A typeless `let` — whose binding never
/// crosses an arm boundary — returns `None` and is left in place.
fn hoist_decl(line: &str) -> Option<(String, String)> {
    let rest = line.strip_prefix("let ")?;
    let rest = rest.strip_prefix("mut ").unwrap_or(rest);
    // `BINDING: TYPE = INIT;` — split at the first top-level ` = `, then split
    // the left side at the first top-level `:`.
    let eq = top_level_find(rest, " = ")?;
    let (lhs, init) = (&rest[..eq], &rest[eq + 3..]);
    let colon = top_level_find(lhs, ":")?;
    let binding = lhs[..colon].trim_end();
    let ty = lhs[colon + 1..].trim();
    let default = type_default(ty);
    let (decl, assign) =
        if let Some(names) = binding.strip_prefix('(').and_then(|b| b.strip_suffix(')')) {
            // A tuple binding. The hoisted declaration needs `mut` on every name
            // (each is reassigned by the arm), while the assignment's LHS pattern
            // must carry none. The source binding may already spell `mut` on some
            // names (batched locals do), so strip it before re-forming each side.
            let bare: Vec<&str> = names
                .split(',')
                .map(|n| n.trim().strip_prefix("mut ").unwrap_or(n.trim()))
                .collect();
            let muts: Vec<String> = bare.iter().map(|n| format!("mut {n}")).collect();
            let decl = format!("let ({}): {ty} = {default};", muts.join(", "));
            let assign = format!("({}) = {init}", bare.join(", "));
            (decl, assign)
        } else {
            let decl = format!("let mut {binding}: {ty} = {default};");
            let assign = format!("{binding} = {init}");
            (decl, assign)
        };
    Some((decl, assign))
}

/// A placeholder default value for `ty`, valid to bind before the real
/// initialising assignment overwrites it. Any value of the type works, since the
/// assignment dominates every read.
fn type_default(ty: &str) -> String {
    if let Some(inner) = ty.strip_prefix('(').and_then(|t| t.strip_suffix(')')) {
        let parts: Vec<String> = inner.split(',').map(|p| type_default(p.trim())).collect();
        format!("({})", parts.join(", "))
    } else if ty == "f32" || ty == "f64" {
        "0.0".to_string()
    } else {
        "0".to_string()
    }
}

/// The byte index of the first occurrence of `pat` in `s` at bracket-nesting
/// depth zero (ignoring matches inside `()`/`[]`/`{}`), if any.
fn top_level_find(s: &str, pat: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut depth = 0i32;
    for i in 0..bytes.len() {
        match bytes[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            _ => {}
        }
        if depth == 0 && s[i..].starts_with(pat) {
            return Some(i);
        }
    }
    None
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
    /// Whether the module declares linear memory (so `self.mem()` exists).
    has_memory: bool,
    /// The number of linear memories (imported first, then defined). Memory
    /// operations carry a static index that must be below this.
    n_memories: usize,
    /// Whether the module declares a table (so `self.table()` exists).
    has_table: bool,
    /// Whether the module has an injected host (`self.imports`), so an external
    /// funcref handle in a table can be resolved through the host.
    has_imports: bool,
    /// The table's element type (`funcref` or `externref`), if a table exists;
    /// `table.get` pushes an operand of this type.
    table_element: Option<ValType>,
    /// Per-data-segment: whether it is passive (so `memory.init`/`data.drop`
    /// can reference it through a `data{d}` field), indexed by data index.
    data_passive: Vec<bool>,
    /// Per-element-segment: whether it is passive (so `table.init`/`elem.drop`
    /// can reference it through an `elem{e}` field), indexed by element index.
    elem_passive: Vec<bool>,
    /// Whether functions are emitted as `&mut self` methods (stateful module).
    is_method: bool,
    /// Per-tag exception payload types, indexed by tag index (imported tags
    /// first, then defined). `throw`/`catch` resolve their tag index here.
    tags: Vec<Vec<ValType>>,
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
    /// with the given comma-separated argument list. A recognised WASI import is
    /// a native inherent method; any other imported function is dispatched
    /// through the injected host; defined ones are (method) calls.
    fn call_expr(&self, fidx: usize, arg_list: &str) -> String {
        if let Some(im) = self.imports.get(fidx) {
            // A recognised WASI import is a native inherent method; any other
            // import is dispatched through the injected host trait.
            return match im.wasi {
                Some(w) => format!("self.{}({arg_list})", w.method()),
                None => format!("self.imports.import{fidx}({arg_list})"),
            };
        }
        if self.is_method {
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
    /// Every linear memory, in index order (imported memories first, then
    /// defined). Empty when the module declares none.
    pub(crate) memories: &'a [MemInfo],
    pub(crate) data: &'a [DataSegment],
    pub(crate) table: Option<&'a TableInfo>,
    pub(crate) elements: &'a [ElemSegment],
    pub(crate) tags: &'a [TagInfo],
}

/// Derive the translation context from a module's raw parts.
///
/// Returns the [`ModuleCtx`] plus whether the module is *stateful* — i.e.
/// carries mutable state (memory, a table, globals or imports) and is therefore
/// emitted as a `struct Instance` with `&mut self` methods rather than free
/// functions. Also performs the module-level validation that cannot be checked
/// during parsing (a native WASI function that reads memory needs one).
fn build_ctx<'a>(parts: &ModuleParts<'a>) -> Result<(ModuleCtx<'a>, bool), TranspileError> {
    let ModuleParts {
        imports,
        imported_globals,
        funcs,
        types,
        globals,
        memories,
        data,
        table,
        elements,
        ..
    } = *parts;

    let has_memory = !memories.is_empty();
    let has_table = table.is_some();
    // A native WASI function that reads/writes linear memory (e.g. `fd_write`)
    // is emitted with `self.mem()`, which only exists when the module has a
    // memory; reject a module that imports one but declares none.
    if !has_memory
        && imports
            .iter()
            .any(|im| im.wasi.is_some_and(WasiFn::needs_memory))
    {
        return Err(TranspileError::Unsupported(
            "native WASI memory access without a linear memory".into(),
        ));
    }
    // The host is injected whenever anything is imported (globals, non-WASI
    // functions, or host-owned memory/table).
    let has_imports = !imports.is_empty()
        || !imported_globals.is_empty()
        || memories.iter().any(|m| m.imported)
        || table.is_some_and(|t| t.imported);
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
        n_memories: memories.len(),
        has_table,
        has_imports,
        table_element: table.map(|t| t.element),
        data_passive: data.iter().map(|d| d.offset.is_none()).collect(),
        elem_passive: elements
            .iter()
            .map(|e| e.offset.is_none() && !e.declared)
            .collect(),
        is_method: stateful,
        tags: parts.tags.iter().map(|t| t.params.clone()).collect(),
    };
    Ok((ctx, stateful))
}

/// Translate a whole module into a single Rust source string.
///
/// A module that declares linear memory, a table or globals carries mutable
/// state, so it is emitted as a `pub struct Instance` with the functions as
/// `&mut self` methods. A stateless module keeps its functions as free
/// `pub fn`s, matching the earlier phases exactly.
pub(crate) fn generate_module(parts: &ModuleParts<'_>) -> Result<String, TranspileError> {
    let (ctx, stateful) = build_ctx(parts)?;

    let mut sources = Vec::with_capacity(parts.funcs.len());
    let mut used: HashSet<(Helper, u32)> = HashSet::new();
    let mut used_rt: HashSet<Rt> = HashSet::new();
    let mut used_simd: HashSet<&'static str> = HashSet::new();
    let mut dispatch_sigs: HashSet<u32> = HashSet::new();
    let mut uses_eh = false;
    for (index, f) in parts.funcs.iter().enumerate() {
        // Defined functions are named by their full function index, i.e. after
        // the imported functions in the shared index space. The single-file
        // path is not memory-critical, so each function is rendered into its own
        // `String` and the pieces are joined/wrapped exactly as before.
        let mut src = String::new();
        let meta = generate_function_into(parts.imports.len() + index, f, &ctx, "", &mut src)?;
        used.extend(meta.helpers);
        used_rt.extend(meta.rt);
        used_simd.extend(meta.simd);
        dispatch_sigs.extend(meta.dispatch_sigs);
        uses_eh |= meta.uses_eh;
        sources.push(src);
    }

    // Free-function runtime helpers live at module scope, above the functions
    // (or the `struct Instance`) that call them, in both module shapes.
    let rt_helpers = render_rt_helpers(&used_rt);
    let simd_helpers = render_simd_helpers(&used_simd);

    let body = if !stateful {
        sources.join("\n")
    } else {
        render_module(parts, &ctx, &sources, &used, &dispatch_sigs)?
    };

    // The exception type, then the runtime helpers, precede the module body.
    let mut prelude = String::new();
    if uses_eh {
        prelude.push_str(EXC_DEF);
        prelude.push('\n');
    }
    prelude.push_str(&rt_helpers);
    prelude.push_str(&simd_helpers);
    Ok(if prelude.is_empty() {
        body
    } else {
        format!("{prelude}\n{body}")
    })
}

/// Translate a module, emitting its Rust source across one or more files.
///
/// Each finished file is handed to `emit(name, code)` and then dropped, so the
/// peak memory stays around one chunk's worth rather than the whole program.
/// When nothing forces a split — the module fits in `funcs_per_file` functions
/// and no `max_bytes_per_file` cap is set — the output is byte-identical to
/// [`generate_module`], emitted as a single `lib.rs`.
///
/// Otherwise the defined functions are chunked into `funcs_{n}.rs` files and a
/// `lib.rs` root ties them together: for a stateless module the chunks hold free
/// `pub fn`s re-exported from the root; for a stateful one each chunk adds an
/// `impl Instance` block while the root owns the struct, `new`, the shared
/// helper methods and the module-scope runtime helpers.
///
/// A chunk is flushed once it reaches `funcs_per_file` functions *or* its
/// accumulated source reaches `max_bytes_per_file` bytes (whichever comes
/// first). The byte cap is what actually bounds peak memory when a module holds
/// a few very large functions, since a fixed function count can still add up to
/// a huge chunk. Both caps take effect only at a function boundary, so a single
/// oversized function is still emitted whole.
pub(crate) fn generate_module_split(
    parts: &ModuleParts<'_>,
    funcs_per_file: usize,
    max_bytes_per_file: usize,
    emit: &mut dyn FnMut(String, String) -> Result<(), TranspileError>,
) -> Result<(), TranspileError> {
    let per = if funcs_per_file == 0 {
        usize::MAX
    } else {
        funcs_per_file
    };
    let byte_cap = if max_bytes_per_file == 0 {
        usize::MAX
    } else {
        max_bytes_per_file
    };
    // With nothing forcing a split, keep the exact single-file rendering.
    if parts.funcs.len() <= per && byte_cap == usize::MAX {
        let code = generate_module(parts)?;
        return emit("lib.rs".to_string(), code);
    }

    let (ctx, stateful) = build_ctx(parts)?;
    let base = parts.imports.len();

    // Aggregated across every function: needed only to render the `lib.rs` root
    // (helper methods, dispatch methods and runtime helpers). Each chunk file is
    // otherwise self-contained, so it can be emitted and dropped immediately.
    let mut used: HashSet<(Helper, u32)> = HashSet::new();
    let mut used_rt: HashSet<Rt> = HashSet::new();
    let mut used_simd: HashSet<&'static str> = HashSet::new();
    let mut dispatch_sigs: HashSet<u32> = HashSet::new();
    let mut uses_eh = false;

    // A stateful chunk wraps its functions in an `impl Instance` block, so every
    // emitted line is indented one level; a stateless chunk emits free `pub fn`s
    // at column zero.
    let line_prefix = if stateful { "    " } else { "" };
    // The current chunk is built in place: its prelude is written when the first
    // function joins it, each function's source is streamed straight in (never
    // buffered as a separate `String`), and the whole buffer is handed to `emit`
    // and reset at a flush. Peak memory is therefore about one chunk, not the
    // whole program.
    let mut chunk = String::new();
    let mut funcs_in_chunk = 0usize;
    let mut chunk_index = 0usize;
    for (index, f) in parts.funcs.iter().enumerate() {
        if funcs_in_chunk == 0 {
            chunk.push_str(&chunk_prelude(parts, stateful));
        }
        // A blank line separates each function from the prelude or its
        // predecessor, matching the single-file rendering.
        chunk.push('\n');
        let meta = generate_function_into(base + index, f, &ctx, line_prefix, &mut chunk)?;
        used.extend(meta.helpers);
        used_rt.extend(meta.rt);
        used_simd.extend(meta.simd);
        dispatch_sigs.extend(meta.dispatch_sigs);
        uses_eh |= meta.uses_eh;
        funcs_in_chunk += 1;

        // Flush at the function count cap or once the chunk's own bytes reach
        // the byte cap (both act only here, at a function boundary).
        if funcs_in_chunk >= per || chunk.len() >= byte_cap {
            if stateful {
                chunk.push_str("}\n");
            }
            emit(
                format!("funcs_{chunk_index}.rs"),
                std::mem::take(&mut chunk),
            )?;
            chunk_index += 1;
            funcs_in_chunk = 0;
        }
    }
    if funcs_in_chunk > 0 {
        if stateful {
            chunk.push_str("}\n");
        }
        emit(
            format!("funcs_{chunk_index}.rs"),
            std::mem::take(&mut chunk),
        )?;
        chunk_index += 1;
    }
    let n_chunks = chunk_index;

    // The root is emitted last, once the used-helper/dispatch sets are complete.
    let deps = render::RootDeps {
        helpers: &used,
        rt: &used_rt,
        simd: &used_simd,
        dispatch_sigs: &dispatch_sigs,
    };
    let root = render_lib_root(parts, &ctx, stateful, &deps, n_chunks)?;
    // The exception type lives at the crate root so every chunk's `use super::*`
    // sees it, ahead of everything else in `lib.rs`.
    let root = if uses_eh {
        format!("{EXC_DEF}\n{root}")
    } else {
        root
    };
    emit("lib.rs".to_string(), root)
}

/// Generate the Rust source of one function, appending it to `out` (each
/// non-empty line prefixed by `line_prefix`, used to indent a method inside a
/// chunk's `impl` block), and return the helper dependencies it discovered.
fn generate_function_into(
    index: usize,
    input: &FuncInput<'_>,
    ctx: &ModuleCtx<'_>,
    line_prefix: &str,
    out: &mut String,
) -> Result<GenMeta, TranspileError> {
    let mut func = FuncGen::new(input.params, input.results, input.body, ctx)?;
    func.run(input.body)?;
    func.finish(index, input.params, input.results, line_prefix, out)
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
        // A `funcref` is a function index and an `externref` is an opaque host
        // handle; both are represented as a `u32` (`u32::MAX` is null), matching
        // the table's element representation.
        ValType::Ref(rt) if rt.is_func_ref() || rt.is_extern_ref() => Ok("u32"),
        // A v128 is a 128-bit value; it is held as a `u128` and lane operations
        // reinterpret its bits (little-endian) into the relevant lane type.
        ValType::V128 => Ok("u128"),
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
        // A default `funcref`/`externref` is null.
        ValType::Ref(rt) if rt.is_func_ref() || rt.is_extern_ref() => "u32::MAX",
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
