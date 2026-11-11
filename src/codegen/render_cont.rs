//! Module-scope rendering for the typed-continuations subsystem: the resumable
//! runtime types (`StepResult`, per-body `ContFrame{N}`, the `ContObj` union)
//! and the `Instance` methods that drive them (`cont_new`/`cont_step`/
//! `cont_bind`). These are emitted only for a module that creates a
//! continuation; `render_module` splices them into the module body. The
//! per-function `cont_step_func{N}` state machines that these types feed are
//! lowered separately (in the `func` submodule).

use super::{CompositeKind, ModuleCtx, default_value, rust_type};
use crate::TranspileError;

/// Bits of a continuation handle that index the slot table (about a million
/// simultaneously live continuations). The handle's remaining low bits hold the
/// slot's generation and — only when the module casts to continuation types — the
/// continuation type's id; bit 31 stays clear (it tags external funcrefs, and
/// `u32::MAX` is null).
const CONT_IDX_BITS: u32 = 20;

/// The module's continuation types (by type index), in id order. With casts the
/// handle carries the id so that a consumed reference still reports its own type.
fn cont_type_indices(ctx: &ModuleCtx<'_>) -> Vec<usize> {
    ctx.type_kinds
        .iter()
        .enumerate()
        .filter(|(_, k)| matches!(k, CompositeKind::Cont(_)))
        .map(|(i, _)| i)
        .collect()
}

/// The generation width of the handle layout `[type | gen | idx]`: whatever bits
/// below bit 31 the slot index and (with casts) the type id leave over.
fn handle_gen_bits(ctx: &ModuleCtx<'_>) -> Result<u32, TranspileError> {
    let type_bits = if ctx.cont.casts {
        let n = cont_type_indices(ctx).len().max(1);
        usize::BITS - (n - 1).leading_zeros()
    } else {
        0
    };
    let gen_bits = 31u32.saturating_sub(CONT_IDX_BITS + type_bits);
    if gen_bits < 2 {
        return Err(TranspileError::Unsupported(
            "too many continuation types to cast between".into(),
        ));
    }
    Ok(gen_bits)
}

/// The module-scope typed-continuations runtime: the `StepResult` a step
/// function returns, one `ContFrame{N}` per continuation-bodied function
/// (holding its resumable state: `pc`, locals, surviving operands, `cont.bind`
/// arguments and any nested checkpoint callee frame), and the `ContObj`
/// union the handle table stores.
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
        // `target`, handing it `args` (the switch payload) and `ty`, the continuation
        // type the parked switcher takes as the target's self-reference; the driving
        // `resume` (its `(on $tag switch)` handler) follows the transfer — see `resume`.
        "    Switch { tag: u32, target: u32, ty: u32, args: Vec<i64> },".to_string(),
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
    lines.push(String::new());
    // One handle-table entry. `epoch` advances whenever the slot's continuation is
    // consumed, so a handle minted for an earlier occupant no longer matches.
    // The frame is boxed so stepping moves a pointer, not the whole `ContObj`.
    let gen_bits = handle_gen_bits(ctx)?;
    lines.extend([
        format!("const CONT_IDX_BITS: u32 = {CONT_IDX_BITS};"),
        format!("const CONT_GEN_BITS: u32 = {gen_bits};"),
        format!("const CONT_IDX_MASK: u32 = (1u32 << {CONT_IDX_BITS}) - 1;"),
        "const CONT_GEN_MASK: u32 = (1u32 << CONT_GEN_BITS) - 1;".to_string(),
    ]);
    if ctx.cont.casts {
        // Where the continuation type id sits in a handle.
        lines.push("const CONT_TYPE_SHIFT: u32 = CONT_IDX_BITS + CONT_GEN_BITS;".to_string());
    }
    lines.push("#[allow(dead_code)]".to_string());
    lines.push("pub struct ContSlot { epoch: u32, obj: Option<Box<ContObj>> }".to_string());
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
/// (`[type | generation | slot index]`, see [`CONT_IDX_BITS`]); `cont_step`
/// resumes the handle once, dispatching to the right step function and — since a
/// continuation is one-shot — keeping the (advanced) object only while it is still
/// suspended; on return the slot is recycled and its generation advanced, so any
/// handle still naming the old occupant traps as consumed.
pub(super) fn continuation_method_lines(ctx: &ModuleCtx<'_>) -> Vec<String> {
    let mut lines = vec![
        "pub fn cont_new(&mut self, __funcidx: u32, __ty: u32) -> u32 {".to_string(),
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
        "    self.cont_alloc(Box::new(__obj), __ty)".to_string(),
        "}".to_string(),
        String::new(),
        // Store `__obj` in a free slot (or a new one) and mint its handle. The last
        // index is never handed out: a null handle (`u32::MAX`) decodes to it.
        "fn cont_alloc(&mut self, __obj: Box<ContObj>, __ty: u32) -> u32 {".to_string(),
        "    let __obj = Some(__obj);".to_string(),
        "    let (__i, __epoch) = match self.cont_free.pop() {".to_string(),
        "        Some(__i) => {".to_string(),
        "            let __s = &mut self.conts[__i as usize];".to_string(),
        "            __s.obj = __obj;".to_string(),
        "            (__i, __s.epoch)".to_string(),
        "        }".to_string(),
        "        None => {".to_string(),
        "            let __i = self.conts.len() as u32;".to_string(),
        "            if __i >= CONT_IDX_MASK { panic!(\"cont.new: continuation table exhausted\"); }"
            .to_string(),
        "            self.conts.push(ContSlot { epoch: 0, obj: __obj });".to_string(),
        "            (__i, 0)".to_string(),
        "        }".to_string(),
        "    };".to_string(),
        "    Self::cont_handle(__i, __epoch, __ty)".to_string(),
        "}".to_string(),
        String::new(),
        // The handle for slot `__i` at generation `__epoch`; with casts it also carries
        // the id of continuation type `__ty`, so a consumed reference keeps its type.
        "fn cont_handle(__i: u32, __epoch: u32, __ty: u32) -> u32 {".to_string(),
    ]);
    if ctx.cont.casts {
        lines.push(
            "    (Self::cont_type_id(__ty) << CONT_TYPE_SHIFT) | (__epoch << CONT_IDX_BITS) | __i"
                .to_string(),
        );
    } else {
        lines.extend([
            "    let _ = __ty;".to_string(),
            "    (__epoch << CONT_IDX_BITS) | __i".to_string(),
        ]);
    }
    lines.extend([
        "}".to_string(),
        String::new(),
        // The live slot behind `__h`: it must still hold its continuation and carry
        // the handle's generation (a recycled slot has moved on).
        "fn cont_slot(&self, __h: u32, __what: &str) -> usize {".to_string(),
        "    let __i = (__h & CONT_IDX_MASK) as usize;".to_string(),
        "    match self.conts.get(__i) {".to_string(),
        "        Some(__s) if __s.obj.is_some() && __s.epoch == ((__h >> CONT_IDX_BITS) & CONT_GEN_MASK) => __i,"
            .to_string(),
        "        _ => panic!(\"{}\", __what),".to_string(),
        "    }".to_string(),
        "}".to_string(),
        String::new(),
        // A returned (or discarded) continuation frees its slot for reuse; the
        // generation advance invalidates every outstanding handle to it. A slot whose
        // generation wraps is retired instead: reusing it would let a very old stale
        // handle match a live one again.
        "fn cont_release(&mut self, __i: usize) {".to_string(),
        "    let __s = &mut self.conts[__i];".to_string(),
        "    __s.obj = None;".to_string(),
        "    __s.epoch = (__s.epoch + 1) & CONT_GEN_MASK;".to_string(),
        "    if __s.epoch != 0 { self.cont_free.push(__i as u32); }".to_string(),
        "}".to_string(),
        String::new(),
        "#[allow(dead_code)]".to_string(),
        "pub fn cont_discard(&mut self, __h: u32) {".to_string(),
        "    let __i = self.cont_slot(__h, \"resume_throw of a consumed continuation\");".to_string(),
        "    self.cont_release(__i);".to_string(),
        "}".to_string(),
        String::new(),
        // `__args` carries the values injected by this resume: at a fresh
        // continuation's first step they are the body's parameters; a
        // parameter-less body simply ignores the (empty) slice.
        "pub fn cont_step(&mut self, __h: u32, __args: &[i64]) -> StepResult {".to_string(),
        "    let __i = self.cont_slot(__h, \"resume of a consumed continuation\");".to_string(),
        "    let mut __obj = self.conts[__i].obj.take().expect(\"resume of a consumed continuation\");"
            .to_string(),
        "    let __r = match &mut *__obj {".to_string(),
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
        // return consumes it and recycles the slot.
        "    if let StepResult::Suspend { .. } | StepResult::Switch { .. } = __r {".to_string(),
        "        self.conts[__i].obj = Some(__obj);".to_string(),
        "    } else {".to_string(),
        "        self.cont_release(__i);".to_string(),
        "    }".to_string(),
        "    __r".to_string(),
        "}".to_string(),
        String::new(),
        // `cont.bind` appends its already-`i64`-erased leading arguments to the
        // continuation's `bound` prefix and hands the (same, in this backend)
        // handle back typed as the partially-applied continuation. The next
        // resume prepends the prefix to its own arguments (see `cont_step`).
        // With casts to continuation types, the bound continuation moves to a new
        // handle (the old reference is consumed and keeps its own type).
        "#[allow(dead_code)]".to_string(),
        "pub fn cont_bind(&mut self, __h: u32, __prefix: &[i64], __ty: u32) -> u32 {".to_string(),
        if ctx.cont.casts {
            "    let __h = self.cont_retype(__h, __ty);".to_string()
        } else {
            "    let _ = __ty;".to_string()
        },
        "    let __i = self.cont_slot(__h, \"cont.bind of a consumed continuation\");".to_string(),
        "    match self.conts[__i].obj.as_deref_mut().expect(\"cont.bind of a consumed continuation\") {"
            .to_string(),
    ]);
    for n in &ctx.cont.bodies {
        lines.push(format!(
            "        ContObj::C{n}(__f) => __f.bound.extend_from_slice(__prefix),"
        ));
    }
    lines.extend(["    }".to_string(), "    __h".to_string(), "}".to_string()]);
    if ctx.cont.casts {
        // The ids of the module's continuation types: what a handle carries so a
        // cast can read the type back without the (possibly recycled) slot.
        let types = cont_type_indices(ctx);
        lines.push(String::new());
        lines.push("fn cont_type_id(__ty: u32) -> u32 {".to_string());
        lines.push("    match __ty {".to_string());
        for (id, ty) in types.iter().enumerate() {
            lines.push(format!("        {ty}u32 => {id}u32,"));
        }
        lines.push("        _ => panic!(\"not a continuation type\"),".to_string());
        lines.push("    }".to_string());
        lines.push("}".to_string());
        lines.push(String::new());
        lines.push("pub fn cont_type_of(&self, __h: u32) -> u32 {".to_string());
        lines.push("    match __h >> CONT_TYPE_SHIFT {".to_string());
        for (id, ty) in types.iter().enumerate() {
            lines.push(format!("        {id}u32 => {ty}u32,"));
        }
        lines.push("        _ => panic!(\"not a continuation handle\"),".to_string());
        lines.push("    }".to_string());
        lines.push("}".to_string());
        lines.extend([
            String::new(),
            // The continuation behind `__h` moves to a fresh handle of type `__ty`; the
            // old handle is consumed (the slot's generation advances) and keeps the type
            // it had.
            "#[allow(dead_code)]".to_string(),
            "pub fn cont_retype(&mut self, __h: u32, __ty: u32) -> u32 {".to_string(),
            "    let __i = self.cont_slot(__h, \"retype of a consumed continuation\");".to_string(),
            "    let __s = &mut self.conts[__i];".to_string(),
            "    __s.epoch = (__s.epoch + 1) & CONT_GEN_MASK;".to_string(),
            // On wrap the slot is retired (never freed) and the continuation moves.
            "    if __s.epoch == 0 {".to_string(),
            "        let __o = __s.obj.take().expect(\"retype of a consumed continuation\");"
                .to_string(),
            "        return self.cont_alloc(__o, __ty);".to_string(),
            "    }".to_string(),
            "    Self::cont_handle(__i as u32, __s.epoch, __ty)".to_string(),
            "}".to_string(),
        ]);
    }
    lines
}

// Fragments that construct and propagate a `StepResult`. The arm-split
// (`func/cont.rs`) and flat (`flatten.rs`) lowerings share them, so a change to
// a `StepResult` variant is made here only.

/// `save` followed by the assignment parking the resume state `pc` in the frame.
pub(super) fn park_pc(save: &str, pc: impl std::fmt::Display) -> String {
    format!("{save}__frame.pc = {pc}u32; ")
}

/// A `StepResult::Suspend` carrying `tag` and the (already encoded) `payload`.
pub(super) fn suspend_result(tag: impl std::fmt::Display, payload: &str) -> String {
    format!("StepResult::Suspend {{ tag: {tag}u32, payload: vec![{payload}] }}")
}

/// A `StepResult::Switch` transferring to `target`; `ty` identifies the
/// switching continuation's own type and `args` is the encoded payload.
pub(super) fn switch_result(
    tag: impl std::fmt::Display,
    target: &str,
    ty: impl std::fmt::Display,
    args: &str,
) -> String {
    format!(
        "StepResult::Switch {{ tag: {tag}u32, target: {target}, ty: {ty}u32, args: vec![{args}] }}"
    )
}

/// The `match` that drives the callee step function `callee` once, forwarding
/// `__args`. `pass` renders the body of the `Suspend`/`Switch` arms from the
/// re-raised result expression (the callee's own result, rebuilt from the
/// bindings); `sep` separates the arms; `on_return` is the `Return` arm's
/// value, which reads the callee's results from `__cret` (always bound; the
/// leading underscores keep it warning-free when the arm ignores it).
pub(super) fn drive_callee_match(
    callee: impl std::fmt::Display,
    pass: impl Fn(&str) -> String,
    sep: &str,
    on_return: &str,
) -> String {
    let suspend = pass("StepResult::Suspend { tag: __t, payload: __p }");
    let switch = pass("StepResult::Switch { tag: __t, target: __tgt, ty: __ty, args: __a }");
    format!(
        "match self.cont_step_func{callee}(&mut __frame.sub, __args) {{ \
         StepResult::Suspend {{ tag: __t, payload: __p }} => {suspend}{sep}\
         StepResult::Switch {{ tag: __t, target: __tgt, ty: __ty, args: __a }} => {switch}{sep}\
         StepResult::Return(__cret) => {on_return} }}"
    )
}
