//! Module-scope rendering for the typed-continuations subsystem: the resumable
//! runtime types (`StepResult`, per-body `ContFrame{N}`, the `ContObj` union)
//! and the `Instance` methods that drive them (`cont_new`/`cont_step`/
//! `cont_bind`). These are emitted only for a module that creates a
//! continuation; `render_module` splices them into the module body. The
//! per-function `cont_step_func{N}` state machines that these types feed are
//! lowered separately (in the `func` submodule).

use super::{ModuleCtx, default_value, rust_type};
use crate::TranspileError;

/// The module-scope typed-continuations runtime: the `StepResult` a step
/// function returns, one `ContFrame{N}` per continuation-bodied function
/// (holding its resumable program counter), and the `ContObj` union the handle
/// table stores. Phase 4 continuation bodies have no parameters or locals, so a
/// frame is just its `pc`.
pub(super) fn continuation_runtime_lines(
    ctx: &ModuleCtx<'_>,
) -> Result<Vec<String>, TranspileError> {
    // A program may create a continuation without resuming it (or vice versa),
    // leaving some fields/variants unexercised, so the generated types carry the
    // same dead-code allowance as the `Instance` struct.
    // The types are `pub` because the `pub` `cont_new`/`cont_step`/`cont_step_func`
    // methods (public so the root impl can reach a chunk's step function) mention
    // them in their signatures.
    let mut lines = vec![
        "#[allow(dead_code)]".to_string(),
        "pub enum StepResult {".to_string(),
        "    Return(Vec<i64>),".to_string(),
        "    Suspend { tag: u32, payload: Vec<i64> },".to_string(),
        // A `switch` parks the running continuation and transfers directly to
        // `target`, handing it `args` (the switch payload); the driving `resume`
        // (its `(on $tag switch)` handler) follows the transfer — see `resume`.
        "    Switch { tag: u32, target: u32, args: Vec<i64> },".to_string(),
        "}".to_string(),
        String::new(),
    ];
    for n in &ctx.cont.step_set {
        // A `ContFrame` holds the resumable `pc`, one field per local (so locals
        // survive suspends), an operand-survivor stack (`ostack`, holding the
        // `i64`-erased operands that outlive a suspend inside a region — see
        // `emit_cont_step_flat`), a `bound` prefix of `i64`-erased arguments
        // supplied ahead of time by `cont.bind` (prepended to the next resume's
        // `__args` — see `cont_step`/`cont_bind`), and — when the function ends in
        // a cross-call checkpoint — the callee's frame nested as `sub`.
        let mut fields = vec![
            "pc: u32".to_string(),
            "ostack: Vec<i64>".to_string(),
            "bound: Vec<i64>".to_string(),
        ];
        if let Some(g) = ctx.cont.checkpoint_callee.get(n) {
            fields.push(format!("sub: ContFrame{g}"));
        }
        if let Some(locals) = ctx.cont.step_locals.get(n) {
            for (i, ty) in locals.iter().enumerate() {
                fields.push(format!("l{i}: {}", rust_type(*ty, ctx.type_kinds)?));
            }
        }
        lines.push("#[allow(dead_code)]".to_string());
        lines.push(format!(
            "pub struct ContFrame{n} {{ {} }}",
            fields.join(", ")
        ));
    }
    lines.push(String::new());
    lines.push("#[allow(dead_code)]".to_string());
    lines.push("pub enum ContObj {".to_string());
    for n in &ctx.cont.bodies {
        lines.push(format!("    C{n}(ContFrame{n}),"));
    }
    lines.push("}".to_string());
    Ok(lines)
}

/// The start-state literal for a step function's frame: `pc` 0, each local at
/// its default, and — when the function ends in a cross-call checkpoint — a
/// freshly-started callee frame in `sub` (recursively). The checkpoint graph is
/// acyclic (a recursive chain is rejected during context building), so this
/// recursion terminates.
fn frame_start_literal(ctx: &ModuleCtx<'_>, n: u32) -> String {
    let mut fields = vec![
        "pc: 0u32".to_string(),
        "ostack: Vec::new()".to_string(),
        "bound: Vec::new()".to_string(),
    ];
    if let Some(g) = ctx.cont.checkpoint_callee.get(&n) {
        fields.push(format!("sub: {}", frame_start_literal(ctx, *g)));
    }
    if let Some(locals) = ctx.cont.step_locals.get(&n) {
        for (i, ty) in locals.iter().enumerate() {
            fields.push(format!("l{i}: {}", default_value(*ty, ctx.type_kinds)));
        }
    }
    format!("ContFrame{n} {{ {} }}", fields.join(", "))
}

/// The `cont_new`/`cont_step` methods on `Instance`. `cont_new` allocates a
/// fresh resumable frame for the referenced function and returns its handle
/// (an index into `conts`); `cont_step` resumes the handle once, dispatching to
/// the right step function and — since a continuation is one-shot — keeping the
/// (advanced) object only while it is still suspended, dropping it on return.
pub(super) fn continuation_method_lines(ctx: &ModuleCtx<'_>) -> Vec<String> {
    let mut lines = vec![
        "pub fn cont_new(&mut self, __funcidx: u32) -> u32 {".to_string(),
        "    let __obj = match __funcidx {".to_string(),
    ];
    for n in &ctx.cont.bodies {
        lines.push(format!(
            "        {n}u32 => ContObj::C{n}({}),",
            frame_start_literal(ctx, *n)
        ));
    }
    lines.extend([
        "        _ => panic!(\"cont.new: not a continuation function\"),".to_string(),
        "    };".to_string(),
        "    self.conts.push(Some(__obj));".to_string(),
        "    (self.conts.len() - 1) as u32".to_string(),
        "}".to_string(),
        String::new(),
        // `__args` carries the values injected by this resume: at a fresh
        // continuation's first step they are the body's parameters; a
        // parameter-less body simply ignores the (empty) slice.
        "pub fn cont_step(&mut self, __h: u32, __args: &[i64]) -> StepResult {".to_string(),
        "    let mut __obj = self.conts[__h as usize]".to_string(),
        "        .take()".to_string(),
        "        .expect(\"resume of a consumed continuation\");".to_string(),
        "    let __r = match &mut __obj {".to_string(),
    ]);
    for n in &ctx.cont.bodies {
        // A continuation carrying `cont.bind`-supplied arguments delivers them
        // ahead of this resume's own `__args`; `bound` is non-empty only until the
        // step that consumes it (it is taken here), so an unbound continuation
        // takes the borrow-free path.
        lines.push(format!("        ContObj::C{n}(__f) => {{"));
        lines.push("            if __f.bound.is_empty() {".to_string());
        lines.push(format!(
            "                self.cont_step_func{n}(__f, __args)"
        ));
        lines.push("            } else {".to_string());
        lines.push("                let mut __a = std::mem::take(&mut __f.bound);".to_string());
        lines.push("                __a.extend_from_slice(__args);".to_string());
        lines.push(format!("                self.cont_step_func{n}(__f, &__a)"));
        lines.push("            }".to_string());
        lines.push("        }".to_string());
    }
    lines.extend([
        "    };".to_string(),
        // A one-shot continuation is kept only while still live: both a suspend
        // and a switch park it (it may be resumed/switched-to again), while a
        // return consumes it.
        "    if let StepResult::Suspend { .. } | StepResult::Switch { .. } = __r {".to_string(),
        "        self.conts[__h as usize] = Some(__obj);".to_string(),
        "    }".to_string(),
        "    __r".to_string(),
        "}".to_string(),
        String::new(),
        // `cont.bind` appends its already-`i64`-erased leading arguments to the
        // continuation's `bound` prefix and hands the (same, in this backend)
        // handle back typed as the partially-applied continuation. The next
        // resume prepends the prefix to its own arguments (see `cont_step`).
        "#[allow(dead_code)]".to_string(),
        "pub fn cont_bind(&mut self, __h: u32, __prefix: &[i64]) -> u32 {".to_string(),
        "    match self.conts[__h as usize]".to_string(),
        "        .as_mut()".to_string(),
        "        .expect(\"cont.bind of a consumed continuation\")".to_string(),
        "    {".to_string(),
    ]);
    for n in &ctx.cont.bodies {
        lines.push(format!(
            "        ContObj::C{n}(__f) => __f.bound.extend_from_slice(__prefix),"
        ));
    }
    lines.extend(["    };".to_string(), "    __h".to_string(), "}".to_string()]);
    lines
}
