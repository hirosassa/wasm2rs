use super::*;

use std::collections::HashMap;

use wasmparser::ValType;

/// A value on the simulated operand stack.
#[derive(Clone)]
pub(crate) struct Val {
    /// The Rust expression that produces this value.
    pub(crate) code: String,
    pub(crate) ty: ValType,
    /// Whether re-evaluating `code` is guaranteed to yield the same result.
    pub(crate) stable: bool,
}
/// The kind of a structured control-flow region.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum FrameKind {
    Block,
    Loop,
    If,
}
/// One active control-flow region.
pub(crate) struct Frame {
    pub(crate) kind: FrameKind,
    /// Numeric label; only rendered as `'lN` if the frame is branched to.
    pub(crate) label: usize,
    /// Set when some `br`/`br_if`/`br_table` targets this frame.
    pub(crate) targeted: bool,
    /// One `(variable, type)` per result the region yields, in source order.
    pub(crate) results: Vec<(String, ValType)>,
    /// The region's entry parameters (stable operands left on the stack), kept
    /// so an `if`'s `else` arm can restore them after the `then` arm consumes
    /// them.
    pub(crate) entry_params: Vec<Val>,
    /// For a `loop`: one `(variable, type)` per parameter. A `br` back to the
    /// header reassigns these loop-carried variables before `continue`. Empty
    /// for blocks and `if`s.
    pub(crate) loop_params: Vec<(String, ValType)>,
    /// Operand-stack height of the enclosing scope (values below this frame).
    pub(crate) parent_height: usize,
    /// The output buffer of the enclosing scope, restored when the frame ends.
    pub(crate) parent_buffer: Vec<Node>,
    /// For `if`: the `then` branch nodes, captured when `else` is reached.
    pub(crate) then_buffer: Option<Vec<Node>>,
    /// For `if`: whether the `then` branch could fall through to `else`.
    pub(crate) then_reachable: bool,
    /// For `if`: the Rust condition expression.
    pub(crate) cond: Option<String>,
    /// For a legacy-exception `try`: the accumulating handler state. A try frame
    /// reuses [`FrameKind::Block`] (a branch to it behaves like a block exit) but
    /// carries this so `end` renders it as a `catch_unwind` region instead.
    pub(crate) try_state: Option<TryState>,
}
/// The kind of one arm of a `try` region.
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum CatchKind {
    /// The protected body.
    Body,
    /// A `catch $tag` handler.
    Tag(u32),
    /// A `catch_all` handler.
    All,
}
/// One finished arm of a `try` region: the body or a catch handler.
pub(crate) struct CatchArm {
    pub(crate) kind: CatchKind,
    /// Prebuilt `let` statements that extract the exception payload into the
    /// handler's operand variables, run before the handler body. Empty for the
    /// body and `catch_all`.
    pub(crate) binds: Vec<String>,
    pub(crate) body: Vec<Node>,
    /// Whether control can fall through this arm's end (so it needs a trailing
    /// `break 'lN;` when the try is targeted).
    pub(crate) reachable_at_end: bool,
}
/// The state accumulated while translating a `try` region, between its opening
/// and its `end`.
pub(crate) struct TryState {
    /// The variable the caught exception box is bound to in the `Err` arm.
    pub(crate) exc_var: String,
    /// Finished arms, in order: the body first, then each catch handler.
    pub(crate) arms: Vec<CatchArm>,
    /// The kind of the arm currently being emitted into `self.cur`.
    pub(crate) cur_kind: CatchKind,
    /// The exception-payload extraction statements of the currently-open catch
    /// handler.
    pub(crate) cur_binds: Vec<String>,
    /// Distinct branch targets that escape this try's body, in discovery order.
    /// The try's outcome variable holds `index + 1` for the branch that fired,
    /// so the post-`match` dispatch can re-issue it outside the closure.
    pub(crate) escapes: Vec<BranchEscape>,
    /// Whether a `return` escapes this try's body (routed through the
    /// function-wide return signal rather than the outcome variable).
    pub(crate) has_ret_escape: bool,
}
/// A branch that leaves a `try` body, recorded so the try's post-`match`
/// dispatch can re-issue it outside the `catch_unwind` closure (as a direct
/// `break`/`continue`, or — when it also escapes an enclosing try — as another
/// closure-outcome signal).
pub(crate) struct BranchEscape {
    /// The frame the branch targets (below the try that it escapes).
    pub(crate) target_idx: usize,
    pub(crate) is_loop: bool,
    pub(crate) label: usize,
}
impl Frame {
    /// The result variable names, in source order.
    pub(crate) fn result_vars(&self) -> Vec<String> {
        self.results.iter().map(|(var, _)| var.clone()).collect()
    }

    /// The loop-carried parameter variable names, in source order.
    pub(crate) fn loop_param_vars(&self) -> Vec<String> {
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
pub(crate) enum Node {
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
    /// A continuation `suspend` point (cont-flatten path only). Splits the `pc`
    /// state machine: the arm saves the mutated locals (`save`), advances
    /// `__frame.pc` to the resume state, and returns `StepResult::Suspend` with
    /// the encoded `payload`. Only ever produced for a body walked by
    /// [`FuncGen::emit_cont_step`] and only consumed by the continuation
    /// flattener; every other `Node` consumer treats it as unreachable.
    Suspend {
        tag: u32,
        payload: String,
        save: String,
    },
    /// A cross-call checkpoint (cont-flatten path only): a tail `call` to another
    /// step function `callee`. Splits the `pc` state machine into a state that
    /// drives `self.cont_step_func{callee}(&mut __frame.sub, &[])` once — on the
    /// callee's suspend it saves the mutated locals (`save`), leaves `__frame.pc`
    /// pinned to this state (so the next resume re-drives the callee), and
    /// propagates the suspension up; on the callee's return it moves the callee's
    /// (already `i64`-erased) results into `__frame.ostack` and advances to the
    /// resume state. Like [`Node::Suspend`], only produced/consumed by the
    /// continuation flattener; every other `Node` consumer treats it as
    /// unreachable.
    Checkpoint { callee: u32, save: String },
    /// A continuation `switch` point (cont-flatten path only). Like
    /// [`Node::Suspend`] it splits the `pc` state machine — saving the mutated
    /// locals (`save`), advancing `__frame.pc` to the switched-back-to state, and
    /// returning out of the step function — but it returns `StepResult::Switch`,
    /// transferring control directly to `target` with the encoded `payload`. The
    /// switched-back-to state receives the self-continuation's parameters as the
    /// next step's `__args`. Only produced/consumed by the continuation
    /// flattener; every other `Node` consumer treats it as unreachable.
    Switch {
        tag: u32,
        target: String,
        payload: String,
        save: String,
    },
}
/// A finished `try` region: the protected body plus its catch handlers.
pub(crate) struct TryRegionNode {
    /// Numeric label; the body is wrapped in `'lN: loop { … }` when `targeted`
    /// so a `br` to the try becomes a `break` out of the protected body.
    pub(crate) label: usize,
    pub(crate) targeted: bool,
    /// The variable the caught exception box binds to in the landing pad.
    pub(crate) exc_var: String,
    /// The protected body.
    pub(crate) body: Vec<Node>,
    /// Whether the body can fall through its end (needs a trailing `break`).
    pub(crate) body_reachable_at_end: bool,
    /// The catch handlers, in source order.
    pub(crate) catches: Vec<CatchArm>,
}
/// One arm of a [`Node::BrTable`]: a match pattern that assigns its target's
/// value-carrying variables then `break`/`continue`s to `label`.
pub(crate) struct BrArm {
    pub(crate) pattern: String,
    pub(crate) label: usize,
    pub(crate) is_loop: bool,
    pub(crate) assigns: Vec<(String, String)>,
}
impl BrArm {
    pub(crate) fn keyword(&self) -> &'static str {
        if self.is_loop { "continue" } else { "break" }
    }
}
/// A finished control-flow region, retained structurally (rather than eagerly
/// flattened to indented text) so it can be rendered nested or flattened.
pub(crate) struct RegionNode {
    pub(crate) kind: FrameKind,
    /// Numeric label; only rendered as `'lN` when `targeted`.
    pub(crate) label: usize,
    /// Whether some `br`/`br_if`/`br_table` targets this region.
    pub(crate) targeted: bool,
    /// Whether control could fall through the region's end (so a targeted
    /// block/loop needs a trailing `break 'lN;`).
    pub(crate) reachable_at_end: bool,
    /// For an `if`: its Rust condition expression; `None` for block/loop.
    pub(crate) cond: Option<String>,
    /// For block/loop: the whole body. For `if`: the `then` arm.
    pub(crate) body: Vec<Node>,
    /// For an `if` with an explicit or implicit `else`: the `else` arm.
    pub(crate) els: Option<Vec<Node>>,
}
/// Render a function body (its deferred [`Node`] list) into `out`, consuming it.
/// Each non-empty line is written as `line_prefix` + the four-space function-body
/// indent + four spaces per control-nesting level + the statement; blank lines
/// stay bare. This reproduces, byte for byte, the previously eager
/// `indent`-based rendering, while consuming and dropping each node as it is
/// copied so a huge body is never held twice.
pub(crate) fn render_body_into(nodes: Vec<Node>, line_prefix: &str, out: &mut String) {
    render_nodes_into(nodes, 0, line_prefix, out);
}
pub(crate) fn render_nodes_into(
    nodes: Vec<Node>,
    depth: usize,
    line_prefix: &str,
    out: &mut String,
) {
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
            // A `suspend`/checkpoint only ever reaches the continuation
            // flattener, never the ordinary nested renderer.
            Node::Suspend { .. } => {
                unreachable!("`suspend` node outside the continuation flattener")
            }
            Node::Checkpoint { .. } => {
                unreachable!("checkpoint node outside the continuation flattener")
            }
            Node::Switch { .. } => {
                unreachable!("`switch` node outside the continuation flattener")
            }
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
pub(crate) fn bool_inner(code: &str) -> Option<&str> {
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
pub(crate) fn render_br_if_nested(
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
pub(crate) fn render_br_table_nested(
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
pub(crate) fn push_body_line(out: &mut String, text: &str, depth: usize, line_prefix: &str) {
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
pub(crate) fn render_region_into(
    region: RegionNode,
    depth: usize,
    line_prefix: &str,
    out: &mut String,
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
/// Render a `try` region as a `catch_unwind` over the protected body followed by
/// a landing pad that dispatches on the caught exception's tag. A thrown wasm
/// exception is a `panic_any` of [`EXC_TYPE`]; any other payload (a trap, or a
/// foreign panic) is re-raised so only wasm exceptions are caught.
pub(crate) fn render_try_into(
    node: TryRegionNode,
    depth: usize,
    line_prefix: &str,
    out: &mut String,
) {
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
pub(crate) fn render_landing_pad(
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
pub(crate) fn estimate_body_len(nodes: &[Node]) -> usize {
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
            Node::Suspend { payload, save, .. } => payload.len() + save.len() + 48,
            Node::Checkpoint { save, .. } => save.len() + 160,
            Node::Switch {
                target,
                payload,
                save,
                ..
            } => target.len() + payload.len() + save.len() + 64,
        })
        .sum()
}
/// Peak control-flow nesting above which a function is emitted as a flat
/// dispatch loop instead of nested Rust, so its rendered nesting cannot overflow
/// rustc's recursive-descent parser. Chosen well above any hand-written nesting
/// (so ordinary functions keep their readable nested form) yet far below the
/// parser's stack limit.
pub(crate) const FLATTEN_DEPTH_THRESHOLD: usize = 40;
/// Whether a body can be lowered to a flat dispatch loop with the currently
/// supported constructs (block/loop/`br`/`br_if`/terminators). `if` regions and
/// `br_table` are not yet flattenable, so a body containing them stays nested.
///
/// Walked with an explicit stack rather than by recursion, so a deeply nested
/// module (the very case that triggers flattening) cannot overflow the thread's
/// stack while deciding whether to flatten.
pub(crate) fn can_flatten(nodes: &[Node]) -> bool {
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
pub(crate) const STRUCT_BUDGET: usize = 32;
/// Whether the flattener can emit `region` as a structured `'lN: loop { … }`
/// rather than dispatch arms: a `loop` that is actually branched back to
/// (`targeted`), whose body nests within [`STRUCT_BUDGET`], and whose subtree
/// the structured renderer can emit (no `try`, no condition-less `if` — the same
/// constructs [`can_flatten`] rejects). Requiring a real back-edge guarantees
/// the structured loop's `'lN` label and `continue 'lN` are used (an unused
/// label would fail `-D warnings`) and keeps the specialisation to loops that
/// actually repeat.
pub(crate) fn is_structurable_loop(region: &RegionNode) -> bool {
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
pub(crate) fn subtree_depth(nodes: &[Node]) -> usize {
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
pub(crate) fn flatten_body(
    nodes: Vec<Node>,
    trailing: Option<String>,
    returns_value: bool,
) -> Vec<Node> {
    let mut f = Flattener {
        arms: Vec::new(),
        next_state: 0,
        labels: HashMap::new(),
        uses_continue: false,
        allow_structured_loop: true,
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
/// Lower a continuation body (already walked into a [`Node`] tree, with
/// `suspend`s marked as [`Node::Suspend`]) to the flat `pc` dispatch of a
/// resumable step function. Mirrors [`flatten_body`] but the dispatch reads its
/// initial `pc` from `__frame.pc` (so a resume re-enters the state a prior
/// suspend saved) and its exit returns `StepResult`, not a plain value.
///
/// `exit_stmt` is the fall-off-the-end terminator (`return
/// StepResult::Return(…)` when the body's end is reachable, else
/// `unreachable!()`). Structured loops are disabled (see
/// [`Flattener::allow_structured_loop`]) and pc-edge contraction is skipped, so
/// every allocated state id is stable — a `suspend` can hard-code the resume
/// state into `__frame.pc`.
pub(crate) fn flatten_cont_body(nodes: Vec<Node>, exit_stmt: String) -> Vec<Node> {
    let mut f = Flattener {
        arms: Vec::new(),
        next_state: 0,
        labels: HashMap::new(),
        uses_continue: false,
        allow_structured_loop: false,
    };
    let start = f.alloc();
    let exit = f.alloc();
    f.arms.push((exit, vec![(0, exit_stmt)]));
    f.lower(nodes, start, exit);
    // The initial state is always 0 (the first `alloc`), matching a fresh
    // frame's `pc == 0`; without contraction the ids never shift.
    debug_assert_eq!(start, 0);
    f.assemble_cont()
}
/// Whether control can reach the code following a finished frame.
pub(crate) fn reachable_after(frame: &Frame, reachable_at_end: bool) -> bool {
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
