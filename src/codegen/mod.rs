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
mod wasi;

use self::func::FuncGen;
use self::helpers::helper_name;
use self::render::{chunk_prelude, render_lib_root, render_module};
use self::runtime::{render_rt_helpers, rt_name};

pub(crate) use self::const_expr::{const_expr_to_rust, const_expr_u32};
pub(crate) use self::info::{
    DataSegment, ElemSegment, FuncInput, GlobalInfo, ImportInfo, ImportedGlobalInfo, MemInfo,
    TableInfo, TypeSig,
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
}

/// The helper dependencies discovered while generating one function.
///
/// The function's own Rust source is written straight into the caller's output
/// buffer (see [`generate_function_into`]) rather than returned, so the whole
/// body is never held twice; only these aggregated sets — needed to render the
/// module/root once every function has been seen — are returned.
struct GenMeta {
    /// Instance-method memory helpers.
    helpers: HashSet<Helper>,
    /// Module-scope free-function runtime helpers.
    rt: HashSet<Rt>,
    /// `call_indirect` type indices needing a `call_ref_t{ti}` dispatch method.
    dispatch_sigs: HashSet<u32>,
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
        }
    }
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
            &format!("if {cond} != 0 {{ {keyword} 'l{label}; }}"),
            depth,
            line_prefix,
        );
    } else {
        push_body_line(out, &format!("if {cond} != 0 {{"), depth, line_prefix);
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
        push_body_line(out, &format!("'l{label}: loop {{"), depth, line_prefix);
        render_inner(out, body, els);
        if reachable_at_end {
            push_body_line(out, &format!("break 'l{label};"), inner_depth, line_prefix);
        }
        push_body_line(out, "}", depth, line_prefix);
    } else {
        render_inner(out, body, els);
    }
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
fn can_flatten(nodes: &[Node]) -> bool {
    nodes.iter().all(|node| match node {
        Node::Line(_)
        | Node::Term(_)
        | Node::Br { .. }
        | Node::BrIf { .. }
        | Node::BrTable { .. } => true,
        Node::Region(region) => match region.kind {
            FrameKind::Block | FrameKind::Loop => can_flatten(&region.body),
            FrameKind::If => {
                region.cond.is_some()
                    && can_flatten(&region.body)
                    && region.els.as_deref().is_none_or(can_flatten)
            }
        },
    })
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
    f.arms.push((exit, vec![exit_stmt]));
    f.lower(nodes, start, exit);
    f.assemble(start)
}

/// Builds the flat `match pc { … }` arms while linearising a structured body.
struct Flattener {
    /// Completed dispatch arms: `(state id, its statements)`.
    arms: Vec<(usize, Vec<String>)>,
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
    fn lower(&mut self, nodes: Vec<Node>, entry: usize, after: usize) {
        let mut state = entry;
        let mut stmts: Vec<String> = Vec::new();
        let mut reachable = true;
        for node in nodes {
            if !reachable {
                // The generator does not emit past a terminator within a region,
                // but skip defensively so a stray node cannot start a dead arm.
                continue;
            }
            match node {
                Node::Line(text) => stmts.push(text),
                Node::Term(text) => {
                    stmts.push(text);
                    self.arms.push((state, std::mem::take(&mut stmts)));
                    reachable = false;
                }
                Node::Br { label, .. } => {
                    let target = self.labels[&label];
                    stmts.push(format!("pc = {target};"));
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
                    let mut line = format!("if {cond} != 0 {{ ");
                    for (var, value) in assigns {
                        line.push_str(&format!("{var} = {value}; "));
                    }
                    line.push_str(&format!("pc = {target}; continue 'sm; }}"));
                    self.uses_continue = true;
                    stmts.push(line);
                }
                Node::Region(region) if region.kind == FrameKind::If => {
                    // Dispatch to the `then`/`else` entry, then rejoin at `cont`.
                    // A branch to the `if` exits to `cont`.
                    let cont = self.alloc();
                    let then_e = self.alloc();
                    self.labels.insert(region.label, cont);
                    let cond = region.cond.clone().unwrap_or_default();
                    let RegionNode { body, els, .. } = region;
                    let else_e = match &els {
                        Some(_) => self.alloc(),
                        None => cont,
                    };
                    stmts.push(format!(
                        "if {cond} {{ pc = {then_e}; }} else {{ pc = {else_e}; }}"
                    ));
                    self.arms.push((state, std::mem::take(&mut stmts)));
                    self.lower(body, then_e, cont);
                    if let Some(els) = els {
                        self.lower(els, else_e, cont);
                    }
                    state = cont;
                }
                Node::Region(region) => {
                    let inner = self.alloc();
                    let cont = self.alloc();
                    // A branch to a loop resumes its header; to a block it exits
                    // to the continuation.
                    let branch_target = match region.kind {
                        FrameKind::Loop => inner,
                        _ => cont,
                    };
                    self.labels.insert(region.label, branch_target);
                    stmts.push(format!("pc = {inner};"));
                    self.arms.push((state, std::mem::take(&mut stmts)));
                    self.lower(region.body, inner, cont);
                    state = cont;
                }
                Node::BrTable { selector, arms } => {
                    // Each arm assigns its target's carried variables then sets
                    // `pc`; the whole `match` ends the current arm.
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
                    stmts.push(line);
                    self.arms.push((state, std::mem::take(&mut stmts)));
                    reachable = false;
                }
            }
        }
        if reachable {
            stmts.push(format!("pc = {after};"));
            self.arms.push((state, stmts));
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
        self.arms.sort_by_key(|(state, _)| *state);

        let mut decls: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut rewritten: Vec<(usize, Vec<String>)> = Vec::with_capacity(self.arms.len());
        for (state, stmts) in self.arms {
            let mut arm = Vec::with_capacity(stmts.len());
            for stmt in stmts {
                match hoist_decl(&stmt) {
                    Some((decl, assign)) => {
                        if seen.insert(decl.clone()) {
                            decls.push(decl);
                        }
                        arm.push(assign);
                    }
                    None => arm.push(stmt),
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
            for stmt in stmts {
                push(3, stmt);
            }
            push(2, "}".to_string());
        }
        push(2, "_ => unreachable!(),".to_string());
        push(1, "}".to_string());
        push(0, "}".to_string());
        out
    }
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
    let decl = if let Some(names) = binding.strip_prefix('(').and_then(|b| b.strip_suffix(')')) {
        // A tuple binding needs `mut` on each name: `let (mut a, mut b): …`.
        let muts: Vec<String> = names
            .split(',')
            .map(|n| format!("mut {}", n.trim()))
            .collect();
        format!("let ({}): {ty} = {default};", muts.join(", "))
    } else {
        format!("let mut {binding}: {ty} = {default};")
    };
    let assign = format!("{binding} = {init}");
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
    pub(crate) memory: Option<&'a MemInfo>,
    pub(crate) data: &'a [DataSegment],
    pub(crate) table: Option<&'a TableInfo>,
    pub(crate) elements: &'a [ElemSegment],
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
        memory,
        data,
        table,
        elements,
        ..
    } = *parts;

    let has_memory = memory.is_some();
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
        || memory.is_some_and(|m| m.imported)
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
        has_table,
        has_imports,
        table_element: table.map(|t| t.element),
        data_passive: data.iter().map(|d| d.offset.is_none()).collect(),
        elem_passive: elements
            .iter()
            .map(|e| e.offset.is_none() && !e.declared)
            .collect(),
        is_method: stateful,
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
    let mut used: HashSet<Helper> = HashSet::new();
    let mut used_rt: HashSet<Rt> = HashSet::new();
    let mut dispatch_sigs: HashSet<u32> = HashSet::new();
    for (index, f) in parts.funcs.iter().enumerate() {
        // Defined functions are named by their full function index, i.e. after
        // the imported functions in the shared index space. The single-file
        // path is not memory-critical, so each function is rendered into its own
        // `String` and the pieces are joined/wrapped exactly as before.
        let mut src = String::new();
        let meta = generate_function_into(parts.imports.len() + index, f, &ctx, "", &mut src)?;
        used.extend(meta.helpers);
        used_rt.extend(meta.rt);
        dispatch_sigs.extend(meta.dispatch_sigs);
        sources.push(src);
    }

    // Free-function runtime helpers live at module scope, above the functions
    // (or the `struct Instance`) that call them, in both module shapes.
    let rt_helpers = render_rt_helpers(&used_rt);

    let body = if !stateful {
        sources.join("\n")
    } else {
        render_module(parts, &ctx, &sources, &used, &dispatch_sigs)?
    };

    Ok(if rt_helpers.is_empty() {
        body
    } else {
        format!("{rt_helpers}\n{body}")
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
    let mut used: HashSet<Helper> = HashSet::new();
    let mut used_rt: HashSet<Rt> = HashSet::new();
    let mut dispatch_sigs: HashSet<u32> = HashSet::new();

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
        dispatch_sigs.extend(meta.dispatch_sigs);
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
    let root = render_lib_root(
        parts,
        &ctx,
        stateful,
        &used,
        &used_rt,
        &dispatch_sigs,
        n_chunks,
    )?;
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
