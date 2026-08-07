use super::*;

use std::collections::{HashMap, HashSet};

/// One statement of a dispatch arm: its indent *relative to the arm body* (0 for
/// an ordinary flat statement; >0 for lines nested inside a structured loop) and
/// the statement text. [`Flattener::assemble`] adds the arm's base indent.
pub(crate) type ArmLine = (usize, String);
/// Builds the flat `match pc { … }` arms while linearising a structured body.
pub(crate) struct Flattener {
    /// Completed dispatch arms: `(state id, its statements)`.
    pub(crate) arms: Vec<(usize, Vec<ArmLine>)>,
    /// Next unused state id.
    pub(crate) next_state: usize,
    /// wasm region label → the state a branch to it jumps to (a loop's header,
    /// a block's continuation).
    pub(crate) labels: HashMap<usize, usize>,
    /// Whether any arm uses `continue 'sm` (emitted for `br_if`), so the
    /// dispatch loop needs its `'sm` label. Without it the label is unused.
    pub(crate) uses_continue: bool,
    /// Whether a back-edge loop may be emitted as a structured `'lN: loop { … }`
    /// (the [`is_structurable_loop`] specialisation). The continuation flattener
    /// disables it: a `suspend` inside a loop must return out of the whole step
    /// function and re-enter through the `pc` dispatch, which a structured Rust
    /// loop cannot express, so continuation loops always flatten.
    pub(crate) allow_structured_loop: bool,
}
impl Flattener {
    pub(crate) fn alloc(&mut self) -> usize {
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
    pub(crate) fn lower(&mut self, nodes: Vec<Node>, entry: usize, after: usize) {
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
                    Node::Region(region)
                        if self.allow_structured_loop && is_structurable_loop(&region) =>
                    {
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
                    Node::Suspend { tag, payload, save } => {
                        // Split the state machine at the suspend: save the mutated
                        // locals, park the resume state in `__frame.pc`, and hand
                        // the suspension up. The code after the suspend continues in
                        // `resume`, entered on the next step call (whose `__args`
                        // are the values the resumer injected).
                        let resume = self.alloc();
                        stmts.push((
                            0,
                            format!(
                                "{save}__frame.pc = {resume}u32; return StepResult::Suspend {{ \
                                 tag: {tag}u32, payload: vec![{payload}] }};"
                            ),
                        ));
                        // The suspend `return`s out of the whole step function (the
                        // dispatch runs inside a `loop`, so an arm value would be
                        // discarded), ending the arm like a `Node::Term`; the
                        // flattener resumes filling the `resume` state's arm.
                        self.arms.push((state, std::mem::take(&mut stmts)));
                        state = resume;
                    }
                    Node::Switch {
                        tag,
                        target,
                        payload,
                        save,
                    } => {
                        // Like `Node::Suspend`, but transfers control to `target`
                        // rather than parking with a payload: save the mutated
                        // locals, record the switched-back-to state in `__frame.pc`,
                        // and hand a `Switch` up (the driving `resume` follows it).
                        // The code after the switch continues in `resume`, whose
                        // `__args` are the self-continuation parameters injected when
                        // control switches back.
                        let resume = self.alloc();
                        stmts.push((
                            0,
                            format!(
                                "{save}__frame.pc = {resume}u32; return StepResult::Switch {{ \
                                 tag: {tag}u32, target: {target}, args: vec![{payload}] }};"
                            ),
                        ));
                        self.arms.push((state, std::mem::take(&mut stmts)));
                        state = resume;
                    }
                    Node::Checkpoint { callee, save } => {
                        // The callee drive is re-entered on every callee-suspend
                        // (the next resume re-drives it), so it must occupy its own
                        // state with nothing before it — otherwise the statements
                        // accumulated ahead of it in this arm would re-run each time.
                        // Close the current state with a jump to a fresh `call` state
                        // that holds only the callee `match`.
                        let call = self.alloc();
                        let resume = self.alloc();
                        stmts.push((0, format!("pc = {call};")));
                        self.arms.push((state, std::mem::take(&mut stmts)));
                        // In the `call` state: drive the callee once, forwarding this
                        // step's `__args`. On the first drive they are ignored (the
                        // callee takes no parameters); on a switch-back or
                        // suspend-resume they carry the injection destined for the
                        // callee — parked at a transfer point below this frame — and
                        // must reach it. The `call` state is never `pc == 0`, so the
                        // header's param prologue does not re-run on re-entry (unlike
                        // the arm-split path, this path needs no guard). On the
                        // callee's suspend, save this frame's mutated locals and
                        // propagate up, leaving `__frame.pc` pinned to `call` so the
                        // next resume re-drives the callee. On its return, move the
                        // callee's `i64`-erased results into `__frame.ostack` (the code
                        // after the call reads them back from there) and advance to
                        // `resume`.
                        self.arms.push((
                            call,
                            vec![(
                                0,
                                format!(
                                    "match self.cont_step_func{callee}(&mut __frame.sub, __args) {{ \
                                     StepResult::Suspend {{ tag: __t, payload: __p }} => {{ \
                                     {save}__frame.pc = {call}u32; \
                                     return StepResult::Suspend {{ tag: __t, payload: __p }}; }} \
                                     StepResult::Switch {{ tag: __t, target: __tgt, args: __a }} => \
                                     {{ {save}__frame.pc = {call}u32; \
                                     return StepResult::Switch {{ tag: __t, target: __tgt, args: __a \
                                     }}; }} \
                                     StepResult::Return(__cret) => {{ __frame.ostack = __cret; \
                                     pc = {resume}; }} }}"
                                ),
                            )],
                        ));
                        state = resume;
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
    pub(crate) fn render_structured_loop(
        &mut self,
        region: RegionNode,
        cont: usize,
    ) -> Vec<ArmLine> {
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
    pub(crate) fn render_structured(
        &mut self,
        nodes: Vec<Node>,
        depth: usize,
        out: &mut Vec<ArmLine>,
    ) {
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
                // The continuation flattener disables structured loops, so a
                // `suspend`/checkpoint never renders through this structured path.
                Node::Suspend { .. } => {
                    unreachable!("`suspend` node inside a structured loop")
                }
                Node::Checkpoint { .. } => {
                    unreachable!("checkpoint node inside a structured loop")
                }
                Node::Switch { .. } => {
                    unreachable!("`switch` node inside a structured loop")
                }
            }
        }
    }

    /// A structured branch to `label`: `continue`/`break 'l{label}` when the
    /// target is a region in this subtree, or `pc = <state>; continue 'sm` when it
    /// is an enclosing flattened region (a state in `self.labels`). `assigns` is a
    /// prebuilt run of `var = value; ` statements that must precede the branch.
    pub(crate) fn structured_branch(
        &mut self,
        label: usize,
        is_loop: bool,
        assigns: &str,
    ) -> String {
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
    pub(crate) fn render_structured_region(
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
    pub(crate) fn assemble(mut self, start: usize) -> Vec<Node> {
        let start = contract_pc_edges(&mut self.arms, start);
        // Fusing never removes a `continue 'sm` (those live only in conditional
        // `br_if` lines or structured-loop arms, neither of which is a linear
        // tail), so `self.uses_continue` stays accurate across the fold.
        fuse_linear_chains(&mut self.arms, start);
        self.render_dispatch(format!("let mut pc: usize = {start};"))
    }

    /// Assemble the dispatch loop of a continuation step function. Unlike
    /// [`Self::assemble`], the initial `pc` is read from the frame (so a resume
    /// re-enters the state a prior suspend parked there) and pc-edge contraction
    /// is skipped, keeping every state id exactly as allocated.
    pub(crate) fn assemble_cont(self) -> Vec<Node> {
        self.render_dispatch("let mut pc: usize = __frame.pc as usize;".to_string())
    }

    /// Shared back end of [`Self::assemble`]/[`Self::assemble_cont`]: sort the
    /// arms, hoist their typed `let`s above the loop, and render `<pc_init>;
    /// <decls>; loop { match pc { … } }`.
    pub(crate) fn render_dispatch(mut self, pc_init: String) -> Vec<Node> {
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
        push(0, pc_init);
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
pub(crate) fn trivial_pc_target(body: &[ArmLine]) -> Option<usize> {
    let [(_, line)] = body else { return None };
    line.strip_prefix("pc = ")?.strip_suffix(';')?.parse().ok()
}
/// Rewrite every `pc = <state>` target in `s` through `map` (leaving all other
/// text untouched). `pc = ` and the digits are ASCII, so verbatim spans are
/// copied at valid UTF-8 boundaries.
pub(crate) fn rewrite_pc_targets(s: &str, map: &impl Fn(usize) -> usize) -> String {
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
pub(crate) fn contract_pc_edges(arms: &mut Vec<(usize, Vec<ArmLine>)>, start: usize) -> usize {
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
/// Append each `pc = <state>` target found in `line` to `out`.
fn collect_pc_targets(line: &str, out: &mut Vec<usize>) {
    let b = line.as_bytes();
    let mut i = 0;
    while i + 5 <= b.len() {
        if &b[i..i + 5] == b"pc = " {
            let ns = i + 5;
            let mut j = ns;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            if j > ns {
                out.push(line[ns..j].parse().unwrap_or(usize::MAX));
                i = j;
                continue;
            }
        }
        i += 1;
    }
}
/// If `body` transfers to exactly one successor, unconditionally, as its final
/// action — i.e. its whole body mentions a single `pc = T` and the last line is
/// the bare `pc = T;` — return `T`. Such a state can be fused into a chain: its
/// predecessor may run its statements inline and continue straight into `T`
/// instead of round-tripping through the `match pc` dispatch. A state that
/// branches (`if …/else …`, a `br_if` plus fall-through, a `br_table`) mentions
/// two or more targets and is excluded; a `pc = T; continue 'sm;` tail (a `br`
/// to an enclosing flattened region, emitted only inside a structured loop) does
/// not match the bare-line shape and is left alone.
fn linear_single_successor(body: &[ArmLine]) -> Option<usize> {
    let total: usize = {
        let mut t = Vec::new();
        for (_, line) in body {
            collect_pc_targets(line, &mut t);
        }
        t.len()
    };
    if total != 1 {
        return None;
    }
    let (_, last) = body.last()?;
    last.strip_prefix("pc = ")?.strip_suffix(';')?.parse().ok()
}
/// Fuse single-successor chains in a flattened dispatch: when a state `S` ends in
/// an unconditional `pc = T;` and `T` is reached from nowhere else, inline `T`'s
/// body at the end of `S` and drop `T`. This removes the jump-table round-trip
/// between the two — the ~55% of a hot flattened function's arms that are plain
/// straight-line edges (measured on the googlesql parser) collapse into their
/// predecessors, shrinking the `match` (faster `rustc`) and cutting the dispatch
/// traffic left after [`contract_pc_edges`] folds the pure forwarders.
///
/// Only runs for [`Flattener::assemble`] (never `assemble_cont`, whose state ids
/// must stay stable for resume). Loops are preserved automatically: a header
/// reached both from outside and by its own back-edge has in-degree ≥ 2 and is
/// never absorbed. Output stays byte-*equivalent* only in behaviour, not text —
/// the point is to change the generated dispatch.
pub(crate) fn fuse_linear_chains(arms: &mut Vec<(usize, Vec<ArmLine>)>, start: usize) {
    // Every state that flows to a single successor unconditionally, and where.
    let linear_succ: HashMap<usize, usize> = arms
        .iter()
        .filter_map(|(s, body)| Some((*s, linear_single_successor(body)?)))
        .collect();
    if linear_succ.is_empty() {
        return;
    }

    // In-degree of each state: one per `pc = N` edge, plus the implicit entry
    // into `start` (so `start` is never absorbed into a predecessor).
    let mut indeg: HashMap<usize, usize> = HashMap::new();
    *indeg.entry(start).or_insert(0) += 1;
    let mut targets = Vec::new();
    for (_, body) in arms.iter() {
        for (_, line) in body {
            targets.clear();
            collect_pc_targets(line, &mut targets);
            for &t in &targets {
                *indeg.entry(t).or_insert(0) += 1;
            }
        }
    }

    // A state is absorbable iff a linear state points to it, that is its only
    // in-edge, it is not the entry, and it is not its own successor (a bare
    // side-effect self-loop, which the in-degree rule already guards against once
    // it is entered from outside — this is belt-and-braces).
    let absorbable: HashSet<usize> = linear_succ
        .values()
        .copied()
        .filter(|t| *t != start && indeg.get(t) == Some(&1) && linear_succ.get(t) != Some(t))
        .collect();
    if absorbable.is_empty() {
        return;
    }

    let order: Vec<usize> = arms.iter().map(|(s, _)| *s).collect();
    let mut body_by_state: HashMap<usize, Vec<ArmLine>> = arms.drain(..).collect();
    let mut consumed: HashSet<usize> = HashSet::new();
    let mut result: Vec<(usize, Vec<ArmLine>)> = Vec::new();
    for s in order {
        if absorbable.contains(&s) || consumed.contains(&s) {
            continue;
        }
        let Some(mut fused) = body_by_state.remove(&s) else {
            continue;
        };
        let mut cur = s;
        while let Some(&t) = linear_succ.get(&cur) {
            if !absorbable.contains(&t) || consumed.contains(&t) {
                break;
            }
            let Some(t_body) = body_by_state.remove(&t) else {
                break;
            };
            fused.pop(); // drop the `pc = t;` terminal now inlined below
            fused.extend(t_body);
            consumed.insert(t);
            cur = t;
        }
        result.push((s, fused));
    }
    *arms = result;
}
/// If `line` is a typed `let` binding, return `(hoisted declaration, in-arm
/// assignment)`: the declaration is placed above the dispatch loop so it stays
/// in scope for every arm, while the assignment (carrying any side effects)
/// stays at the original program point. A typeless `let` — whose binding never
/// crosses an arm boundary — returns `None` and is left in place.
pub(crate) fn hoist_decl(line: &str) -> Option<(String, String)> {
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
pub(crate) fn type_default(ty: &str) -> String {
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
pub(crate) fn top_level_find(s: &str, pat: &str) -> Option<usize> {
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

#[cfg(test)]
mod tests {
    use super::{ArmLine, fuse_linear_chains};

    fn line(s: &str) -> ArmLine {
        (0, s.to_string())
    }

    fn texts(body: &[ArmLine]) -> Vec<&str> {
        body.iter().map(|(_, s)| s.as_str()).collect()
    }

    #[test]
    fn fuses_a_straight_line_chain_into_one_arm() {
        // 0 --(side effect)--> 1 --(side effect)--> 2 (returns). States 1 and 2
        // each have a single in-edge, so both collapse into 0.
        let mut arms = vec![
            (0, vec![line("a += 1;"), line("pc = 1;")]),
            (1, vec![line("a += 2;"), line("pc = 2;")]),
            (2, vec![line("return a;")]),
        ];
        fuse_linear_chains(&mut arms, 0);
        assert_eq!(arms.len(), 1);
        assert_eq!(arms[0].0, 0);
        assert_eq!(texts(&arms[0].1), ["a += 1;", "a += 2;", "return a;"]);
    }

    #[test]
    fn keeps_a_join_point_with_two_predecessors() {
        // Both the branch's `else` (state 0) and the forwarder (state 1) target
        // state 2, so state 2 has in-degree 2 and must not be absorbed. State 1
        // is only reached through the branch (not a *linear* predecessor), so it
        // is not absorbable either. Nothing fuses.
        let mut arms = vec![
            (0, vec![line("if c { pc = 1; } else { pc = 2; }")]),
            (1, vec![line("a += 1;"), line("pc = 2;")]),
            (2, vec![line("return a;")]),
        ];
        fuse_linear_chains(&mut arms, 0);
        assert_eq!(arms.len(), 3);
    }

    #[test]
    fn preserves_a_loop_entered_from_outside() {
        // 0 flows into loop header 1; 1's back-edge (`pc = 1;`) targets itself, so
        // header 1 has in-degree 2 (entry + back-edge) and is not absorbed. The
        // infinite loop survives rather than being folded away.
        let mut arms = vec![
            (0, vec![line("a = 5;"), line("pc = 1;")]),
            (1, vec![line("a += 1;"), line("pc = 1;")]),
        ];
        fuse_linear_chains(&mut arms, 0);
        assert_eq!(arms.len(), 2);
        assert_eq!(texts(&arms[1].1), ["a += 1;", "pc = 1;"]);
    }

    #[test]
    fn never_absorbs_the_start_state() {
        // State 1 forwards to the entry (state 0). Even though 0 has a single
        // `pc = 0` in-edge, the implicit entry keeps its in-degree at 2, so it is
        // preserved (folding it would leave the machine with no entry arm).
        let mut arms = vec![
            (0, vec![line("a += 1;"), line("pc = 1;")]),
            (1, vec![line("a += 2;"), line("pc = 0;")]),
        ];
        fuse_linear_chains(&mut arms, 0);
        // 1 is absorbable (single in-edge from 0, linear), so it folds into 0;
        // the resulting self-loop keeps 0 as the sole, entry-preserving arm.
        assert_eq!(arms.len(), 1);
        assert_eq!(arms[0].0, 0);
        assert_eq!(texts(&arms[0].1), ["a += 1;", "a += 2;", "pc = 0;"]);
    }
}
