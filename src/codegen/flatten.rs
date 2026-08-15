use super::*;

use std::collections::{BTreeMap, HashMap, HashSet};

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
    /// When set (and the flattened dispatch has more arms than
    /// [`SplitPlan::max_arms`]), split the dispatch into sibling part functions
    /// over a shared state struct instead of rendering one giant function. The
    /// continuation flattener leaves this `None` (its step functions are already
    /// bounded and resume through `__frame.pc`).
    pub(crate) split: Option<SplitPlan>,
}
/// The enclosing function's signature, threaded into the flattener so a split
/// dispatch can synthesise the shared state struct, the part functions and the
/// trampoline driver that match it.
pub(crate) struct SplitPlan {
    /// Cap on match arms per part function (`0` disables the split).
    pub(crate) max_arms: usize,
    /// The function index `N`, naming `S{N}` / `func{N}_part{k}`.
    pub(crate) func_index: usize,
    /// Whether the function is a `&mut self` method (its parts take `&mut self`
    /// and the state struct lives at module scope) or a free function.
    pub(crate) is_method: bool,
    /// The function parameters as `(local index, Rust type)`; each becomes a
    /// state-struct field `l{index}` the driver initialises from its argument.
    pub(crate) params: Vec<(usize, String)>,
    /// The Rust return type (`None` for a unit-returning function), wrapped as
    /// `Option<_>` in each part's signature.
    pub(crate) ret: Option<String>,
}
/// The pieces a (possibly split) dispatch renders to: the driver body that goes
/// inside `func{N}`, the sibling part functions, and — when split — the shared
/// state struct's definition (emitted at module scope). For an un-split dispatch
/// `siblings` and `state_struct` are empty.
pub(crate) struct DispatchArtifacts {
    /// The driver body placed inside `func{N}` (rendered like any function body).
    pub(crate) body: Vec<Node>,
    /// The sibling part functions, each element one line at column-0-relative
    /// indentation; the caller prepends its `line_prefix` (empty for a free
    /// function, one level for a method inside its `impl`).
    pub(crate) siblings: Vec<String>,
    /// The shared state struct definition (multi-line, module scope); empty when
    /// the dispatch was not split.
    pub(crate) state_struct: String,
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
    pub(crate) fn assemble(mut self, start: usize) -> DispatchArtifacts {
        let start = contract_pc_edges(&mut self.arms, start);
        // Fusing never removes a `continue 'sm` (those live only in conditional
        // `br_if` lines or structured-loop arms, neither of which is a linear
        // tail), so `self.uses_continue` stays accurate across the fold.
        fuse_linear_chains(&mut self.arms, start);
        // A large flattened dispatch is split into sibling part functions so the
        // Rust backend optimises each independently; a small one (or when the
        // split is disabled) renders whole.
        match self.split.take() {
            Some(plan) if plan.max_arms > 0 && self.arms.len() > plan.max_arms => {
                self.render_split(start, &plan)
            }
            _ => DispatchArtifacts {
                body: self.render_dispatch(format!("let mut pc: usize = {start};")),
                siblings: Vec::new(),
                state_struct: String::new(),
            },
        }
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

    /// Render a large flattened dispatch as several sibling *part* functions over
    /// a shared state struct, plus the trampoline `func{N}` driver body. Every
    /// arm's locals/temps (and `pc`) become fields of `S{N}`; each part runs a
    /// contiguous slice of the (renumbered) states and returns `None` once `pc`
    /// leaves its slice, so the driver bounces control to the owning part. A
    /// terminating arm's `return x;` becomes `return Some(x);`, threaded back out
    /// through the driver.
    fn render_split(self, start: usize, plan: &SplitPlan) -> DispatchArtifacts {
        let uses_continue = self.uses_continue;
        let mut arms = self.arms;
        arms.sort_by_key(|(state, _)| *state);

        // Hoist typed `let`s just as `render_dispatch` hoists them above the loop
        // (dedup by full declaration text); each becomes a state-struct field.
        let mut decls: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut hoisted: Vec<(usize, Vec<ArmLine>)> = Vec::with_capacity(arms.len());
        for (state, stmts) in arms {
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
            hoisted.push((state, arm));
        }

        // Renumber the surviving states to a dense `0..N` in sorted order, so part
        // `k` owns the contiguous range `[k*max, (k+1)*max)` and `pc / max` selects
        // the owning part in O(1).
        let renum: HashMap<usize, usize> = hoisted
            .iter()
            .enumerate()
            .map(|(i, (state, _))| (*state, i))
            .collect();
        let n = hoisted.len();
        let max = plan.max_arms;
        let nparts = n.div_ceil(max);
        let start = renum[&start];

        // How each state name is spelled inside a relocated arm: `repl[name]` is the
        // full replacement expression (already `st.`-qualified). Lookups are by exact
        // token text (membership, not a `v\d+`-style pattern), so a SIMD `v128`
        // helper or any lookalike token is never touched. `pc` and the parameters
        // stay individual fields; the hoisted locals/temps are banked (below).
        let mut repl: HashMap<String, String> = HashMap::new();
        repl.insert("pc".to_string(), "st.pc".to_string());
        let mut named: Vec<(String, String)> = Vec::new();
        for (idx, ty) in &plan.params {
            let name = format!("l{idx}");
            if repl.insert(name.clone(), format!("st.{name}")).is_none() {
                named.push((name, ty.clone()));
            }
        }
        // Bank the Copy-typed hoisted temps into one array per Rust type, so a
        // function that hoists thousands of temps yields a handful of struct fields
        // (and a `Default` of a few array-repeats) instead of one field — and one
        // `Default::default()` — per temp, which made rustc's typeck super-linear.
        // A non-Copy `GcRef` temp can't ride an array-repeat, so it stays a field.
        let mut bank_count: BTreeMap<String, usize> = BTreeMap::new();
        for decl in &decls {
            for (name, ty) in parse_hoisted_decl(decl) {
                if repl.contains_key(&name) {
                    continue;
                }
                if is_bankable(&ty) {
                    let idx = bank_count.entry(ty.clone()).or_insert(0);
                    repl.insert(name, format!("st.bank_{ty}[{idx}]"));
                    *idx += 1;
                } else {
                    repl.insert(name.clone(), format!("st.{name}"));
                    named.push((name, ty));
                }
            }
        }

        // Rewrite every arm body — renumber its `pc = N` targets, prefix state
        // references with `st.`, wrap `return` values in `Some(...)` — and group
        // the arms by their part index.
        let mut parts: Vec<Vec<(usize, Vec<ArmLine>)>> = vec![Vec::new(); nparts];
        for (state, stmts) in hoisted {
            let ns = renum[&state];
            let rewritten: Vec<ArmLine> = stmts
                .into_iter()
                .map(|(indent, line)| {
                    let line = rewrite_pc_targets(&line, &|s| renum[&s]);
                    let line = rewrite_state_refs(&line, &repl);
                    (indent, wrap_returns(&line))
                })
                .collect();
            parts[ns / max].push((ns, rewritten));
        }

        let ret = plan.ret.as_deref().unwrap_or("()");
        let sname = format!("S{}", plan.func_index);

        // The shared state struct (emitted at module scope by the caller): `pc`, the
        // individual `pc`/param/`GcRef` fields, then one array bank per Copy temp
        // type. The hand-written `Default` initialises each bank with a single
        // array-repeat (`[Default::default(); N]`, valid for the `Copy` scalar temp
        // types) so `..Default::default()` in the driver still reproduces every
        // hoisted `let mut … = <default>` initial value — without a per-temp field.
        let mut state_struct = String::new();
        state_struct.push_str("#[allow(dead_code)]\n");
        state_struct.push_str(&format!("struct {sname} {{\n"));
        state_struct.push_str("    pc: usize,\n");
        for (name, ty) in &named {
            state_struct.push_str(&format!("    {name}: {ty},\n"));
        }
        for (ty, count) in &bank_count {
            state_struct.push_str(&format!("    bank_{ty}: [{ty}; {count}],\n"));
        }
        state_struct.push_str("}\n");
        state_struct.push_str(&format!("impl Default for {sname} {{\n"));
        state_struct.push_str("    fn default() -> Self {\n");
        state_struct.push_str("        Self {\n");
        state_struct.push_str("            pc: 0,\n");
        for (name, _ty) in &named {
            state_struct.push_str(&format!("            {name}: Default::default(),\n"));
        }
        for (ty, count) in &bank_count {
            state_struct.push_str(&format!(
                "            bank_{ty}: [Default::default(); {count}],\n"
            ));
        }
        state_struct.push_str("        }\n");
        state_struct.push_str("    }\n");
        state_struct.push_str("}\n");

        // Each part function: a bounded `loop { match st.pc { … } }` over its own
        // states; any `pc` outside the slice returns `None` to the driver. Lines
        // are indented relative to column 0; the caller adds its `line_prefix`.
        let self_param = if plan.is_method { "&mut self, " } else { "" };
        let loop_head = if uses_continue {
            "'sm: loop {"
        } else {
            "loop {"
        };
        let mut siblings: Vec<String> = Vec::new();
        for (k, states) in parts.iter().enumerate() {
            if !plan.is_method {
                siblings.push(ALLOW.to_string());
            }
            siblings.push(format!(
                "fn func{}_part{k}({self_param}st: &mut {sname}) -> Option<{ret}> {{",
                plan.func_index
            ));
            siblings.push(format!("    {loop_head}"));
            siblings.push("        match st.pc {".to_string());
            for (ns, stmts) in states {
                siblings.push(format!("            {ns} => {{"));
                for (indent, stmt) in stmts {
                    siblings.push(format!("{}{stmt}", "    ".repeat(4 + indent)));
                }
                siblings.push("            }".to_string());
            }
            siblings.push("            _ => return None,".to_string());
            siblings.push("        }".to_string());
            siblings.push("    }".to_string());
            siblings.push("}".to_string());
        }

        // The driver body placed inside `func{N}`: seed the state from the params,
        // then trampoline between parts until one returns a value.
        let mut body: Vec<Node> = Vec::new();
        let mut push_body = |depth: usize, line: String| {
            body.push(Node::Line(format!("{}{line}", "    ".repeat(depth))));
        };
        let inits: String = plan
            .params
            .iter()
            .map(|(idx, _)| format!("l{idx}, "))
            .collect();
        push_body(
            0,
            format!("let mut st = {sname} {{ pc: {start}, {inits}..Default::default() }};"),
        );
        push_body(0, "loop {".to_string());
        let callee = if plan.is_method { "self." } else { "" };
        push_body(1, format!("let __step = match st.pc / {max} {{"));
        for k in 0..nparts {
            push_body(
                2,
                format!("{k} => {callee}func{}_part{k}(&mut st),", plan.func_index),
            );
        }
        push_body(2, "_ => unreachable!(),".to_string());
        push_body(1, "};".to_string());
        push_body(1, "if let Some(__ret) = __step {".to_string());
        push_body(2, "return __ret;".to_string());
        push_body(1, "}".to_string());
        push_body(0, "}".to_string());

        DispatchArtifacts {
            body,
            siblings,
            state_struct,
        }
    }
}
/// Parse a hoisted declaration into the `(name, type)` of every local it binds.
/// [`hoist_decl`] emits two shapes: a single `let mut <name>: <type> = <default>;`
/// (one pair) and a batched tuple `let (mut a, mut b): (T, U) = <default>;` (one
/// pair per element — these MUST be recovered too, or the tuple's locals get no
/// state slot and dangle as unresolved names). Returns an empty vec for any other
/// line, so only real hoisted state becomes struct fields.
fn parse_hoisted_decl(decl: &str) -> Vec<(String, String)> {
    let Some(rest) = decl.strip_prefix("let ") else {
        return Vec::new();
    };
    let Some(eq) = top_level_find(rest, " = ") else {
        return Vec::new();
    };
    let lhs = &rest[..eq];
    let Some(colon) = top_level_find(lhs, ":") else {
        return Vec::new();
    };
    let binding = lhs[..colon].trim();
    let ty = lhs[colon + 1..].trim();
    // A batched tuple binding: `(mut a, mut b): (T, U)`. Pair each name with its
    // element type positionally (both split naively on `,`, matching `hoist_decl`,
    // which is safe because every wasm value type is a single comma-free token).
    if let Some(names) = binding.strip_prefix('(').and_then(|b| b.strip_suffix(')'))
        && let Some(types) = ty.strip_prefix('(').and_then(|t| t.strip_suffix(')'))
    {
        return names
            .split(',')
            .zip(types.split(','))
            .map(|(n, t)| {
                let n = n.trim().strip_prefix("mut ").unwrap_or(n.trim());
                (n.to_string(), t.trim().to_string())
            })
            .collect();
    }
    let name = binding.strip_prefix("mut ").unwrap_or(binding).trim();
    vec![(name.to_string(), ty.to_string())]
}
/// Whether `c` can start a Rust identifier.
fn is_ident_start(c: u8) -> bool {
    c == b'_' || c.is_ascii_alphabetic()
}
/// Whether `c` can continue a Rust identifier.
fn is_ident_continue(c: u8) -> bool {
    c == b'_' || c.is_ascii_alphanumeric()
}
/// The byte length of a UTF-8 code point from its leading byte.
fn utf8_len(lead: u8) -> usize {
    match lead {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}
/// Whether a hoisted temp of Rust type `ty` can ride a Copy array bank. Only the
/// scalar value-type lowerings qualify (`[x; N]` array-repeat needs `Copy`); a
/// non-Copy `GcRef` handle falls back to an individual struct field.
fn is_bankable(ty: &str) -> bool {
    matches!(ty, "i32" | "i64" | "f32" | "f64" | "u32" | "u128")
}
/// Rewrite every whole-identifier occurrence of a state name in `line` to its
/// shared-state expression (`repl[name]`, e.g. `st.pc` or `st.bank_i32[7]`) so a
/// relocated arm reads/writes the shared struct. Only tokens whose exact text is a
/// key in `repl` are touched (never a substring, a field access after `.`, a
/// `'label`, or anything inside a string literal), so lookalikes like the SIMD
/// helper token `v128` are safe.
fn rewrite_state_refs(line: &str, repl: &HashMap<String, String>) -> String {
    let b = line.as_bytes();
    let n = b.len();
    let mut out = String::with_capacity(line.len() + 16);
    let mut i = 0;
    while i < n {
        let c = b[i];
        // Copy a string literal verbatim (its contents are never rewritten).
        if c == b'"' {
            let start = i;
            i += 1;
            while i < n {
                match b[i] {
                    b'\\' => i = (i + 2).min(n),
                    b'"' => {
                        i += 1;
                        break;
                    }
                    _ => i += 1,
                }
            }
            out.push_str(&line[start..i]);
            continue;
        }
        if is_ident_start(c) {
            let start = i;
            i += 1;
            while i < n && is_ident_continue(b[i]) {
                i += 1;
            }
            let word = &line[start..i];
            let prev = if start == 0 { 0 } else { b[start - 1] };
            // Not a field access, not a `'label`, not mid-identifier.
            if prev != b'.'
                && prev != b'\''
                && !is_ident_continue(prev)
                && let Some(expr) = repl.get(word)
            {
                out.push_str(expr);
            } else {
                out.push_str(word);
            }
            continue;
        }
        let len = utf8_len(c);
        out.push_str(&line[i..(i + len).min(n)]);
        i += len;
    }
    out
}
/// Whether `needle` occurs in `b` at `i` as a whole word (not preceded/followed
/// by an identifier character).
fn matches_word(b: &[u8], i: usize, needle: &[u8]) -> bool {
    if i + needle.len() > b.len() || &b[i..i + needle.len()] != needle {
        return false;
    }
    let before = if i == 0 { 0 } else { b[i - 1] };
    let after = b.get(i + needle.len()).copied().unwrap_or(0);
    !is_ident_continue(before) && !is_ident_continue(after)
}
/// Wrap each `return <expr>;` (or bare `return;`) in `line` as `return
/// Some(<expr>);` (or `return Some(());`), so a part function that reaches the
/// original function's exit yields the value to the trampoline driver. Matches
/// only a whole `return` keyword outside string literals, so `__returning` and a
/// quoted `"return"` are left alone.
fn wrap_returns(line: &str) -> String {
    let b = line.as_bytes();
    let n = b.len();
    let mut out = String::with_capacity(line.len() + 8);
    let mut i = 0;
    while i < n {
        let c = b[i];
        if c == b'"' {
            let start = i;
            i += 1;
            while i < n {
                match b[i] {
                    b'\\' => i = (i + 2).min(n),
                    b'"' => {
                        i += 1;
                        break;
                    }
                    _ => i += 1,
                }
            }
            out.push_str(&line[start..i]);
            continue;
        }
        if matches_word(b, i, b"return") {
            let after = i + 6;
            match b.get(after).copied() {
                // `return;` — a unit return.
                Some(b';') => {
                    out.push_str("return Some(());");
                    i = after + 1;
                    continue;
                }
                // `return <expr>;` — wrap the expression up to its terminator.
                Some(b' ') => {
                    let expr_start = after + 1;
                    if let Some(off) = line[expr_start..].find(';') {
                        let end = expr_start + off;
                        out.push_str("return Some(");
                        out.push_str(&line[expr_start..end]);
                        out.push_str(");");
                        i = end + 1;
                        continue;
                    }
                }
                _ => {}
            }
        }
        let len = utf8_len(c);
        out.push_str(&line[i..(i + len).min(n)]);
        i += len;
    }
    out
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
    use std::collections::HashMap;

    use super::{
        ArmLine, fuse_linear_chains, is_bankable, parse_hoisted_decl, rewrite_state_refs,
        wrap_returns,
    };

    fn line(s: &str) -> ArmLine {
        (0, s.to_string())
    }

    /// The name→access map `render_split` feeds [`rewrite_state_refs`]: `pc` and a
    /// param stay plain fields, a banked temp becomes an array index. `l3405` is a
    /// local whose textual form also appears as a `'l3405` loop label below — the
    /// case the `'`-skip exists for.
    fn state_repl() -> HashMap<String, String> {
        [
            ("pc", "st.pc"),
            ("l0", "st.l0"),
            ("l4", "st.bank_i32[7]"),
            ("l3405", "st.l3405"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
    }

    #[test]
    fn rewrite_state_refs_rewrites_only_whole_word_state_tokens() {
        let r = state_repl();
        // Bare whole-word state names become their shared-state access; a banked
        // temp indexes its array.
        assert_eq!(
            rewrite_state_refs("pc = l4;", &r),
            "st.pc = st.bank_i32[7];"
        );
        // A lookalike that merely *contains* a state name is a different identifier
        // and is left alone; the real `l4` beside it is still rewritten.
        assert_eq!(
            rewrite_state_refs("let pcx = l4 + l40;", &r),
            "let pcx = st.bank_i32[7] + l40;"
        );
    }

    #[test]
    fn rewrite_state_refs_skips_field_access_labels_and_string_contents() {
        let r = state_repl();
        // A token after `.` is a field access, not a state read — so rewriting is
        // idempotent (`st.pc` must not become `st.st.pc`).
        assert_eq!(rewrite_state_refs("st.pc = 3;", &r), "st.pc = 3;");
        // A `'label` that spells a state name must survive: `'l3405` is a loop
        // label, not the local `l3405`. Rewriting it would produce invalid Rust.
        assert_eq!(
            rewrite_state_refs("if c { pc = 5; continue 'l3405; }", &r),
            "if c { st.pc = 5; continue 'l3405; }"
        );
        // A state name inside a string literal is data, not code.
        assert_eq!(
            rewrite_state_refs(r#"trace("pc l4"); pc = 1;"#, &r),
            r#"trace("pc l4"); st.pc = 1;"#
        );
    }

    #[test]
    fn wrap_returns_wraps_real_returns_and_leaves_lookalikes() {
        // A value return is wrapped for the trampoline; a bare return yields unit.
        assert_eq!(wrap_returns("return l4;"), "return Some(l4);");
        assert_eq!(wrap_returns("return f(a, b);"), "return Some(f(a, b));");
        assert_eq!(wrap_returns("return;"), "return Some(());");
        // `return` as a substring of another identifier is not the keyword.
        assert_eq!(wrap_returns("let x = __returning;"), "let x = __returning;");
        // `return` inside a string literal is data, left verbatim.
        assert_eq!(
            wrap_returns(r#"panic!("return here");"#),
            r#"panic!("return here");"#
        );
    }

    #[test]
    fn is_bankable_covers_copy_scalars_and_excludes_gcref() {
        // Every Copy scalar lowering rides an array bank (`[x; N]` needs `Copy`).
        for ty in ["i32", "i64", "f32", "f64", "u32", "u128"] {
            assert!(is_bankable(ty), "{ty} should be bankable");
        }
        // A non-Copy `GcRef` cannot ride an array-repeat and must fall back to an
        // individual field; an unknown type is conservatively non-bankable too.
        assert!(!is_bankable("GcRef"));
        assert!(!is_bankable("bool"));
    }

    #[test]
    fn parse_hoisted_decl_reads_single_and_batched_tuple_bindings() {
        // A single binding yields its one `(name, type)`.
        assert_eq!(
            parse_hoisted_decl("let mut l7: i32 = 0;"),
            vec![("l7".to_string(), "i32".to_string())]
        );
        // A batched tuple binding (the shape `hoist_decl` emits for grouped locals)
        // must yield every element paired with its type — regression guard for the
        // dropped-local bug where a tuple's names got no state slot and dangled.
        assert_eq!(
            parse_hoisted_decl("let (mut l3, mut l4, mut l5): (i32, i64, f32) = (0, 0, 0.0);"),
            vec![
                ("l3".to_string(), "i32".to_string()),
                ("l4".to_string(), "i64".to_string()),
                ("l5".to_string(), "f32".to_string()),
            ]
        );
        // A non-declaration line contributes nothing.
        assert!(parse_hoisted_decl("l4 = st.bank_i32[7];").is_empty());
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
