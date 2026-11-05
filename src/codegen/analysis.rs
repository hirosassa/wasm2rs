use super::*;

use std::collections::{HashMap, HashSet};

use wasmparser::{AbstractHeapType, HeapType, Operator, ValType};

use crate::TranspileError;

/// Whether the module declares at least one struct or array type, so the
/// managed value model ([`GCREF_DEF`]) must be emitted at module scope.
pub(crate) fn declares_gc_types(type_kinds: &[CompositeKind]) -> bool {
    type_kinds
        .iter()
        .any(|k| matches!(k, CompositeKind::Struct(_) | CompositeKind::Array(_)))
}
/// Whether the managed value model ([`GCREF_DEF`]) must be emitted: either the
/// module declares a struct/array type, or a body introduces an abstract GC
/// reference (e.g. `ref.null any`), which also lowers to a `GcRef` even without
/// any concrete GC type declared.
pub(crate) fn needs_gc_types(parts: &ModuleParts<'_>) -> Result<bool, TranspileError> {
    Ok(declares_gc_types(parts.type_kinds) || uses_abstract_gc(parts.funcs)?)
}
/// Whether any function body contains an operator matching `pred`.
///
/// Reads a fresh operator reader per body (`get_operators_reader` yields an
/// independent reader, so this does not disturb the real codegen walk later),
/// short-circuiting on the first match.
pub(crate) fn any_body_op(
    funcs: &[FuncInput<'_>],
    pred: impl Fn(&Operator<'_>) -> bool,
) -> Result<bool, TranspileError> {
    for input in funcs {
        for op in input.body.get_operators_reader()? {
            if pred(&op?) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}
/// Whether any function body creates an abstract GC reference without a concrete
/// struct/array type being declared: either `ref.null` of an abstract GC heap
/// type (`any`/`eq`/`i31`/`struct`/`array`/`none`), `ref.i31` (which produces a
/// `GcRef::I31`), or a convert op (which produces/consumes a managed `anyref`).
/// All force the managed value model to be emitted even with no struct/array
/// type declared.
pub(crate) fn uses_abstract_gc(funcs: &[FuncInput<'_>]) -> Result<bool, TranspileError> {
    any_body_op(funcs, |op| {
        matches!(op, Operator::RefNull { hty } if abstract_is_gc(*hty))
            || matches!(
                op,
                Operator::RefI31 | Operator::AnyConvertExtern | Operator::ExternConvertAny
            )
    })
}
/// Everything `build_ctx` needs from a single pass over every function body.
///
/// Previously each of these was a separate full scan of all functions' operators
/// (`uses_call_ref`, `uses_array_segment_ops`, `uses_extern_convert`,
/// `continuation_bodies`, plus the suspend/call collection inside
/// `can_suspend_functions` and again in `step_functions`). Most modules use no
/// continuations or GC at all, yet paid for every scan. `scan_bodies` folds them
/// into one traversal; the booleans and per-function tables it returns feed the
/// statefulness decision and the continuation fixpoints downstream.
pub(crate) struct BodyScan {
    /// Per defined function (0-based): the locals it writes with `local.set`/
    /// `local.tee`. A written local is unstable (a read of it cannot be reordered
    /// past a write) and needs a `mut` binding.
    pub(crate) mutated: Vec<LocalFlags>,
    /// Continuation bodies: the target of a `ref.func` immediately followed by
    /// `cont.new` (the phase-4 pattern), sorted and deduplicated.
    pub(crate) cont_bodies: Vec<u32>,
    /// Per defined function (0-based): whether its own body has a `suspend`/
    /// `switch` control transfer. Seeds the `can_suspend` fixpoint.
    pub(crate) suspends: Vec<bool>,
    /// Per defined function (0-based): the callees of its direct
    /// `call`/`return_call` edges (full index space). Shared by the `can_suspend`
    /// and `step_functions` fixpoints so neither re-scans.
    pub(crate) calls: Vec<Vec<u32>>,
    /// Any body uses `extern.convert_any`/`any.convert_extern` (bridges the
    /// `extern`/`any` hierarchies through the per-instance `extern_box`).
    pub(crate) uses_extern_box: bool,
    /// Any body uses `call_ref`/`return_call_ref` (dispatches through a
    /// `self.call_ref_t{ti}` method).
    pub(crate) uses_call_ref: bool,
    /// Any body uses an array segment operator (`array.new_data`/`init_data`/
    /// `new_elem`/`init_elem`, which read the retained passive segments).
    pub(crate) uses_array_segment_ops: bool,
}

/// A per-function flag for each local index, a dense replacement for a
/// `HashSet<u32>` (it is queried on every `local.get`). Indices past the
/// function's local count are never set and read as unset.
#[derive(Clone, Default)]
pub(crate) struct LocalFlags(Vec<bool>);

impl LocalFlags {
    fn with_len(n: usize) -> Self {
        Self(vec![false; n])
    }

    fn insert(&mut self, index: u32) {
        if let Some(flag) = self.0.get_mut(index as usize) {
            *flag = true;
        }
    }

    pub(crate) fn contains(&self, index: u32) -> bool {
        self.0.get(index as usize).copied().unwrap_or(false)
    }
}

/// Scan every function body exactly once, collecting all the whole-module
/// operator facts `build_ctx` needs (see [`BodyScan`]). Each of the flagged
/// operators forces the module to be emitted as a `struct Instance` because it
/// reaches per-instance state that free functions cannot.
pub(crate) fn scan_bodies(funcs: &[FuncInput<'_>]) -> Result<BodyScan, TranspileError> {
    let mut scan = BodyScan {
        mutated: Vec::with_capacity(funcs.len()),
        cont_bodies: Vec::new(),
        suspends: vec![false; funcs.len()],
        calls: vec![Vec::new(); funcs.len()],
        uses_extern_box: false,
        uses_call_ref: false,
        uses_array_segment_ops: false,
    };
    for (i, input) in funcs.iter().enumerate() {
        let mut n_locals = input.params.len();
        for local in input.body.get_locals_reader()? {
            let (count, _) = local?;
            n_locals = n_locals.saturating_add(count as usize);
        }
        let mut mutated = LocalFlags::with_len(n_locals);
        // `ref.func` immediately followed by `cont.new` names a continuation
        // body; any other operator breaks the adjacency, so every non-matching
        // arm clears `last_ref_func`.
        let mut last_ref_func: Option<u32> = None;
        for op in input.body.get_operators_reader()? {
            match op? {
                Operator::RefFunc { function_index } => {
                    last_ref_func = Some(function_index);
                    continue;
                }
                Operator::ContNew { .. } => {
                    if let Some(f) = last_ref_func {
                        scan.cont_bodies.push(f);
                    }
                }
                Operator::LocalSet { local_index } | Operator::LocalTee { local_index } => {
                    mutated.insert(local_index);
                }
                Operator::Suspend { .. } | Operator::Switch { .. } => scan.suspends[i] = true,
                Operator::Call { function_index } | Operator::ReturnCall { function_index } => {
                    scan.calls[i].push(function_index);
                }
                Operator::AnyConvertExtern | Operator::ExternConvertAny => {
                    scan.uses_extern_box = true;
                }
                Operator::CallRef { .. } | Operator::ReturnCallRef { .. } => {
                    scan.uses_call_ref = true;
                }
                Operator::ArrayNewData { .. }
                | Operator::ArrayInitData { .. }
                | Operator::ArrayNewElem { .. }
                | Operator::ArrayInitElem { .. } => scan.uses_array_segment_ops = true,
                _ => {}
            }
            last_ref_func = None;
        }
        scan.mutated.push(mutated);
    }
    scan.cont_bodies.sort_unstable();
    scan.cont_bodies.dedup();
    Ok(scan)
}
/// The functions that can transitively reach a control transfer (`suspend` or
/// `switch`): either directly in their own body, or through a `call`/`return_call`
/// to another function that can. Computed as a fixpoint over the direct-call graph
/// (full index space). A caller reaching one must itself become a step function so
/// the transfer can propagate up through a cross-call checkpoint.
///
/// Indirect edges (`call_indirect`/`call_ref`) are deliberately ignored: a step
/// function may not cross them (they are rejected), and a continuation step
/// function is barred from appearing in an element segment, so no indirect edge
/// can reach one. Imported functions never transfer (they have no body).
pub(crate) fn can_suspend_functions(
    n_imports: usize,
    suspends: &[bool],
    calls: &[Vec<u32>],
) -> Result<HashSet<u32>, TranspileError> {
    let mut can: HashSet<u32> = HashSet::new();
    for (i, &s) in suspends.iter().enumerate() {
        if s {
            can.insert(index_u32(n_imports + i)?);
        }
    }
    loop {
        let mut changed = false;
        for (i, callees) in calls.iter().enumerate() {
            let fidx = index_u32(n_imports + i)?;
            if !can.contains(&fidx) && callees.iter().any(|g| can.contains(g)) {
                can.insert(fidx);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    Ok(can)
}
/// The functions emitted as resumable `cont_step_func{N}` state machines: every
/// continuation body, plus every function transitively reachable from one
/// through a suspend-crossing `call` (a call to a function that can suspend).
///
/// Also returns, per step function, the callee of its single tail cross-call
/// checkpoint (the last `call` to another step function), used to nest that
/// callee's frame inside the caller's. A recursive checkpoint chain would give
/// an infinitely-nested frame, so it is rejected here.
pub(crate) fn step_functions(
    n_imports: usize,
    cont_bodies: &[u32],
    can_suspend: &HashSet<u32>,
    calls: &[Vec<u32>],
) -> Result<(Vec<u32>, HashMap<u32, u32>), TranspileError> {
    let defined_index = |f: u32| (f as usize).checked_sub(n_imports);

    let mut step: std::collections::BTreeSet<u32> = cont_bodies.iter().copied().collect();
    loop {
        let mut changed = false;
        for f in step.iter().copied().collect::<Vec<_>>() {
            let Some(di) = defined_index(f) else { continue };
            for &g in &calls[di] {
                if can_suspend.contains(&g) && step.insert(g) {
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }

    // The tail checkpoint is the *first* call to another step function, matching
    // `begin_checkpoint`, which accepts the first and rejects any later one (only
    // one cross-call checkpoint is allowed per body).
    let mut checkpoint: HashMap<u32, u32> = HashMap::new();
    for &f in &step {
        let Some(di) = defined_index(f) else { continue };
        if let Some(&g) = calls[di].iter().find(|g| step.contains(g)) {
            checkpoint.insert(f, g);
        }
    }
    // A checkpoint chain that loops back would nest frames without bound.
    for &start in checkpoint.keys() {
        let mut cur = start;
        for _ in 0..=step.len() {
            match checkpoint.get(&cur) {
                Some(&next) => cur = next,
                None => break,
            }
        }
        if checkpoint.contains_key(&cur) {
            return Err(TranspileError::Unsupported(
                "recursive continuation call chain: a cross-call checkpoint that \
                 loops back would nest frames without bound"
                    .into(),
            ));
        }
    }

    Ok((step.into_iter().collect(), checkpoint))
}
/// The local types (by index) of each step function, so `render` can give each
/// `ContFrame` a field per local. Step functions have no parameters (rejected
/// at emit time), so these are the declared locals starting at index 0.
pub(crate) fn step_function_locals(
    funcs: &[FuncInput<'_>],
    n_imports: usize,
    step_set: &[u32],
) -> Result<HashMap<u32, Vec<ValType>>, TranspileError> {
    let mut map = HashMap::new();
    for &f in step_set {
        let Some(di) = (f as usize).checked_sub(n_imports) else {
            continue;
        };
        let Some(input) = funcs.get(di) else { continue };
        let mut locals = input.params.to_vec();
        for local in input.body.get_locals_reader()? {
            let (count, ty) = local?;
            for _ in 0..count {
                locals.push(ty);
            }
        }
        map.insert(f, locals);
    }
    Ok(map)
}
/// Reject uses of a continuation step function that would need an ordinary
/// `func{N}` method. A step function is emitted only as a resumable
/// `cont_step_func{N}`, so a direct `call`/`return_call` from a *non-step*
/// function, a `ref.func` used for anything but the immediately-following
/// `cont.new`, or an element-segment entry (which feeds `call_indirect`) would
/// all reference a method that is never emitted. A `call` from *within* another
/// step function is the legitimate cross-call checkpoint and is allowed.
pub(crate) fn reject_dual_use_continuations(
    funcs: &[FuncInput<'_>],
    elements: &[ElemSegment],
    step_set: &[u32],
    n_imports: usize,
) -> Result<(), TranspileError> {
    if step_set.is_empty() {
        return Ok(());
    }
    let is_step = |f: u32| step_set.binary_search(&f).is_ok();
    for (i, input) in funcs.iter().enumerate() {
        let container_is_step = is_step(index_u32(n_imports + i)?);
        // A `ref.func` awaiting its consumer; legitimate only when the very next
        // operator is the `cont.new` that turns it into a continuation handle.
        let mut pending_ref_func: Option<u32> = None;
        for op in input.body.get_operators_reader()? {
            let op = op?;
            if let Some(f) = pending_ref_func.take()
                && is_step(f)
                && !matches!(op, Operator::ContNew { .. })
            {
                return Err(TranspileError::Unsupported(
                    "continuation step function used as a plain funcref".into(),
                ));
            }
            match op {
                Operator::RefFunc { function_index } => pending_ref_func = Some(function_index),
                Operator::Call { function_index } | Operator::ReturnCall { function_index }
                    if is_step(function_index) && !container_is_step =>
                {
                    return Err(TranspileError::Unsupported(
                        "continuation step function is called directly outside a continuation"
                            .into(),
                    ));
                }
                _ => {}
            }
        }
    }
    for seg in elements {
        if seg.funcs.iter().any(|f| is_step(*f)) {
            return Err(TranspileError::Unsupported(
                "continuation step function appears in an element segment".into(),
            ));
        }
    }
    Ok(())
}
pub(crate) fn rust_type(
    ty: ValType,
    kinds: &[CompositeKind],
) -> Result<&'static str, TranspileError> {
    match ty {
        ValType::I32 => Ok("i32"),
        ValType::I64 => Ok("i64"),
        ValType::F32 => Ok("f32"),
        ValType::F64 => Ok("f64"),
        // A `funcref` is a function index and an `externref` is an opaque host
        // handle; both are represented as a `u32` (`u32::MAX` is null), matching
        // the table's element representation.
        ValType::Ref(rt)
            if rt.is_func_ref() || rt.is_extern_ref() || is_exn_heap(rt.heap_type()) =>
        {
            Ok("u32")
        }
        // An abstract GC reference (`any`/`eq`/`i31`/`struct`/`array`/`none`) is a
        // managed `GcRef` handle. An `i31ref` rides as the `GcRef::I31` variant so
        // it can flow through an `anyref`/`eqref` alongside heap objects.
        ValType::Ref(rt) if abstract_is_gc(rt.heap_type()) => Ok("GcRef"),
        // A concrete typed reference `(ref $t)` / `(ref null $t)`. A funcref-typed
        // one lowers to `u32` (a function index) like the abstract `funcref`,
        // while a struct/array-typed one is a managed `GcRef` handle.
        ValType::Ref(rt) if matches!(rt.heap_type(), HeapType::Concrete(_)) => {
            if concrete_is_gc(rt.heap_type(), kinds) {
                Ok("GcRef")
            } else {
                Ok("u32")
            }
        }
        // A v128 is a 128-bit value; it is held as a `u128` and lane operations
        // reinterpret its bits (little-endian) into the relevant lane type.
        ValType::V128 => Ok("u128"),
        other => Err(TranspileError::Unsupported(format!("value type {other:?}"))),
    }
}
/// Whether a heap type is the abstract `exn` type: an `exnref` is a `u32` handle
/// into the exception store.
pub(crate) fn is_exn_heap(hty: HeapType) -> bool {
    matches!(
        hty,
        HeapType::Abstract {
            ty: AbstractHeapType::Exn,
            ..
        }
    )
}
/// Whether a heap type is one of the abstract GC heap types (`any`/`eq`/`i31`/
/// `struct`/`array`/`none`), which lower to the managed `GcRef` value model. The
/// abstract `func`/`nofunc`/`extern`/`noextern` types are deliberately excluded
/// (they keep the `u32` lowering). `i31` is included: its payload rides as
/// `GcRef::I31` so it unifies with the `any`/`eq` hierarchy.
pub(crate) fn abstract_is_gc(hty: HeapType) -> bool {
    matches!(
        hty,
        HeapType::Abstract {
            ty: AbstractHeapType::Any
                | AbstractHeapType::Eq
                | AbstractHeapType::I31
                | AbstractHeapType::Struct
                | AbstractHeapType::Array
                | AbstractHeapType::None,
            ..
        }
    )
}
/// Whether a concrete heap type names a struct or array type (a managed `GcRef`)
/// rather than a function type (a `u32` funcref). Unknown or non-module indices
/// conservatively fall back to the funcref lowering.
pub(crate) fn concrete_is_gc(hty: HeapType, kinds: &[CompositeKind]) -> bool {
    let HeapType::Concrete(idx) = hty else {
        return false;
    };
    let Some(module_idx) = idx.as_module_index() else {
        return false;
    };
    matches!(
        kinds.get(module_idx as usize),
        Some(CompositeKind::Struct(_) | CompositeKind::Array(_))
    )
}
/// The Rust type name of each value type, in order.
pub(crate) fn rust_types(
    tys: &[ValType],
    kinds: &[CompositeKind],
) -> Result<Vec<&'static str>, TranspileError> {
    tys.iter().map(|ty| rust_type(*ty, kinds)).collect()
}
/// The unsigned integer type used to reinterpret `ty` for unsigned operations.
pub(crate) fn unsigned_type(ty: ValType) -> Result<&'static str, TranspileError> {
    match ty {
        ValType::I32 => Ok("u32"),
        ValType::I64 => Ok("u64"),
        other => Err(TranspileError::Unsupported(format!(
            "unsigned operation on {other:?}"
        ))),
    }
}
pub(crate) fn default_value(ty: ValType, kinds: &[CompositeKind]) -> &'static str {
    match ty {
        ValType::F32 | ValType::F64 => "0.0",
        // A struct/array reference, or an abstract GC type (`any`/`eq`/`i31`/…),
        // defaults to the managed null handle.
        ValType::Ref(rt)
            if concrete_is_gc(rt.heap_type(), kinds) || abstract_is_gc(rt.heap_type()) =>
        {
            "GcRef::Null"
        }
        // A default `funcref`/`externref` (and any concrete typed funcref) is
        // null.
        ValType::Ref(rt)
            if rt.is_func_ref()
                || rt.is_extern_ref()
                || is_exn_heap(rt.heap_type())
                || matches!(rt.heap_type(), HeapType::Concrete(_)) =>
        {
            "u32::MAX"
        }
        _ => "0",
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use wasmparser::{FunctionBody, Parser, Payload};

    /// The bodies of every function in the module `wat`, in order.
    fn bodies(wasm: &[u8]) -> Vec<FunctionBody<'_>> {
        Parser::new(0)
            .parse_all(wasm)
            .filter_map(|p| match p.unwrap() {
                Payload::CodeSectionEntry(b) => Some(b),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn scan_records_the_locals_each_function_writes() {
        let wasm = wat::parse_str(
            r#"(module
                (func (param i32) (local i32 i32)
                  (local.set 1 (i32.const 1))
                  (drop (local.tee 0 (i32.const 2))))
                (func (param i32) (local i32)
                  (drop (local.get 0)) (drop (local.get 1)))
                (func (local i32 i32 i32)
                  (block (loop (local.set 2 (i32.const 7)) (br 1)))))"#,
        )
        .unwrap();
        let bodies = bodies(&wasm);
        let inputs: Vec<FuncInput<'_>> = bodies
            .iter()
            .map(|body| FuncInput {
                params: &[],
                results: &[],
                body,
            })
            .collect();

        let scan = scan_bodies(&inputs).unwrap();

        let written =
            |f: usize| -> Vec<u32> { (0..8).filter(|&l| scan.mutated[f].contains(l)).collect() };
        assert_eq!(written(0), vec![0, 1]);
        assert_eq!(written(1), Vec::<u32>::new());
        assert_eq!(written(2), vec![2]);
        // A local index past anything the function writes is simply not mutated.
        assert!(!scan.mutated[2].contains(1_000_000));
    }
}
