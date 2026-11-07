//! The canonical ABI's model of a [`WitType`]: its size and alignment in linear memory,
//! its flattened form as core wasm values, and the core signature of a lowered function
//! (`canonical-abi/Explainer.md`: `elem_size`, `alignment`, `flatten_type`, `join`,
//! `flatten_functype`).
//!
//! Types are shared (`Arc`) the way the component shares them, so every walk spends a
//! [`Budget`] and refuses a type that expands past it instead of running exponentially.

use crate::TranspileError;
use crate::wit::{NODE_BUDGET, WitFunc, WitType};

/// A core wasm value type a WIT value is flattened to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Flat {
    I32,
    I64,
    F32,
    F64,
}

/// `MAX_FLAT_PARAMS`: more flat parameters than this are passed through memory.
pub(crate) const MAX_FLAT_PARAMS: usize = 16;
/// `MAX_FLAT_RESULTS`: more flat results than this are returned through memory.
pub(crate) const MAX_FLAT_RESULTS: usize = 1;

/// How many type nodes a walk may still visit.
pub(crate) struct Budget(usize);

impl Budget {
    pub(crate) fn new() -> Self {
        Budget(NODE_BUDGET)
    }

    pub(crate) fn spend(&mut self) -> Result<(), TranspileError> {
        self.0 = self.0.checked_sub(1).ok_or_else(|| {
            TranspileError::Unsupported(format!(
                "type too large (it expands to more than {NODE_BUDGET} type nodes)"
            ))
        })?;
        Ok(())
    }
}

/// The core type that both `a` and `b` are held in (`join` in the spec).
fn join(a: Flat, b: Flat) -> Flat {
    match (a, b) {
        _ if a == b => a,
        (Flat::I32, Flat::F32) | (Flat::F32, Flat::I32) => Flat::I32,
        _ => Flat::I64,
    }
}

/// The size in bytes (and alignment) of the discriminant of a variant with `cases` cases.
fn discriminant(cases: usize) -> u64 {
    match cases {
        0..=0x100 => 1,
        0x101..=0x1_0000 => 2,
        _ => 4,
    }
}

/// The payload types of a variant-like type's cases (an enum's are all absent), or `None`
/// for a type that is not variant-like.
fn cases_of(ty: &WitType) -> Option<Vec<Option<&WitType>>> {
    match ty {
        WitType::Option(t) => Some(vec![None, Some(t.as_ref())]),
        WitType::Result { ok, err } => Some(vec![ok.as_deref(), err.as_deref()]),
        WitType::Variant(v) => Some(v.cases.iter().map(|(_, t)| t.as_deref()).collect()),
        WitType::Enum(e) => Some(e.cases.iter().map(|_| None).collect()),
        _ => None,
    }
}

fn align_to(ptr: u64, align: u64) -> u64 {
    // `align` is a power of two, at least 1.
    let align = align.max(1);
    ptr.saturating_add(align.saturating_sub(1)) / align * align
}

/// The size and alignment in bytes of `ty` in linear memory.
pub(crate) fn size_align(ty: &WitType, budget: &mut Budget) -> Result<(u64, u64), TranspileError> {
    budget.spend()?;
    Ok(match ty {
        WitType::Bool | WitType::U8 | WitType::S8 => (1, 1),
        WitType::U16 | WitType::S16 => (2, 2),
        WitType::U32 | WitType::S32 | WitType::F32 | WitType::Char => (4, 4),
        WitType::U64 | WitType::S64 | WitType::F64 => (8, 8),
        WitType::String | WitType::List(_) => (8, 4),
        WitType::Own(_) | WitType::Borrow(_) => (4, 4),
        WitType::Tuple(ts) => {
            let mut fields = Vec::new();
            for t in ts {
                fields.push(size_align(t, budget)?);
            }
            record_layout(&fields)
        }
        WitType::Record(r) => {
            let mut fields = Vec::new();
            for (_, t) in &r.fields {
                fields.push(size_align(t, budget)?);
            }
            record_layout(&fields)
        }
        WitType::Flags(f) => match f.flags.len() {
            0 => (0, 1),
            1..=8 => (1, 1),
            9..=16 => (2, 2),
            n => (
                u64::try_from(n.div_ceil(32))
                    .unwrap_or(u64::MAX)
                    .saturating_mul(4),
                4,
            ),
        },
        _ => {
            // Variant-like: the discriminant, then the largest payload, aligned.
            let cases = cases_of(ty).unwrap_or_default();
            let disc = discriminant(cases.len());
            let (mut max_size, mut max_align) = (0u64, 1u64);
            for t in cases.into_iter().flatten() {
                let (s, a) = size_align(t, budget)?;
                max_size = max_size.max(s);
                max_align = max_align.max(a);
            }
            let align = disc.max(max_align);
            let size = align_to(align_to(disc, max_align).saturating_add(max_size), align);
            (size, align)
        }
    })
}

/// The offset of each field of a record (or tuple) of `types` in memory: every field at its
/// alignment, one after the other.
pub(crate) fn field_offsets(
    types: &[&WitType],
    budget: &mut Budget,
) -> Result<Vec<u64>, TranspileError> {
    let mut offsets = Vec::new();
    let mut at = 0u64;
    for t in types {
        let (size, align) = size_align(t, budget)?;
        at = align_to(at, align);
        offsets.push(at);
        at = at.saturating_add(size);
    }
    Ok(offsets)
}

/// For a variant-like type (option, result, variant, enum): the size of its discriminant and
/// the offset of its payload (`align_to(disc, max case alignment)`); `None` for other types.
pub(crate) fn variant_layout(
    ty: &WitType,
    budget: &mut Budget,
) -> Result<Option<(u64, u64)>, TranspileError> {
    let Some(cases) = cases_of(ty) else {
        return Ok(None);
    };
    let disc = discriminant(cases.len());
    let mut max_align = 1u64;
    for t in cases.into_iter().flatten() {
        max_align = max_align.max(size_align(t, budget)?.1);
    }
    Ok(Some((disc, align_to(disc, max_align))))
}

/// A record's layout from its fields' `(size, align)`: each field at its alignment, the
/// whole aligned to the largest field alignment.
pub(crate) fn record_layout(fields: &[(u64, u64)]) -> (u64, u64) {
    let align = fields.iter().map(|&(_, a)| a).max().unwrap_or(1);
    let mut size = 0u64;
    for &(s, a) in fields {
        size = align_to(size, a).saturating_add(s);
    }
    (align_to(size, align), align)
}

/// The core values `ty` flattens to (`flatten_type`). A variant is its discriminant then
/// the payloads of its cases overlaid slot by slot, each slot the [`join`] of what the
/// cases put there.
pub(crate) fn flatten(ty: &WitType, budget: &mut Budget) -> Result<Vec<Flat>, TranspileError> {
    budget.spend()?;
    let mut out = Vec::new();
    match ty {
        WitType::Bool
        | WitType::U8
        | WitType::U16
        | WitType::U32
        | WitType::S8
        | WitType::S16
        | WitType::S32
        | WitType::Char
        | WitType::Own(_)
        | WitType::Borrow(_) => out.push(Flat::I32),
        WitType::U64 | WitType::S64 => out.push(Flat::I64),
        WitType::F32 => out.push(Flat::F32),
        WitType::F64 => out.push(Flat::F64),
        WitType::String | WitType::List(_) => out.extend([Flat::I32, Flat::I32]),
        WitType::Tuple(ts) => {
            for t in ts {
                out.extend(flatten(t, budget)?);
            }
        }
        WitType::Record(r) => {
            for (_, t) in &r.fields {
                out.extend(flatten(t, budget)?);
            }
        }
        WitType::Flags(f) => {
            out.extend(std::iter::repeat_n(Flat::I32, f.flags.len().div_ceil(32)));
        }
        _ => {
            let mut joined: Vec<Flat> = Vec::new();
            for t in cases_of(ty).unwrap_or_default().into_iter().flatten() {
                for (i, ft) in flatten(t, budget)?.into_iter().enumerate() {
                    match joined.get_mut(i) {
                        Some(slot) => *slot = join(*slot, ft),
                        None => joined.push(ft),
                    }
                }
            }
            out.push(Flat::I32);
            out.extend(joined);
        }
    }
    Ok(out)
}

/// Whether a value of `ty` is, or holds, a `string` or `list`: something the canonical ABI
/// passes as a pointer into linear memory.
pub(crate) fn needs_memory(ty: &WitType, budget: &mut Budget) -> Result<bool, TranspileError> {
    budget.spend()?;
    Ok(match ty {
        WitType::String | WitType::List(_) => true,
        WitType::Tuple(ts) => any(ts.iter().map(AsRef::as_ref), budget)?,
        WitType::Record(r) => any(r.fields.iter().map(|(_, t)| t.as_ref()), budget)?,
        _ => any(
            cases_of(ty).unwrap_or_default().into_iter().flatten(),
            budget,
        )?,
    })
}

fn any<'a>(
    types: impl Iterator<Item = &'a WitType>,
    budget: &mut Budget,
) -> Result<bool, TranspileError> {
    for t in types {
        if needs_memory(t, budget)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The core signature of the import that lowers a function (`flatten_functype` for
/// `lower`): the flattened parameters, or a single pointer when there are more than
/// [`MAX_FLAT_PARAMS`]; the flattened result, or a trailing return pointer parameter and
/// no result when it has more than [`MAX_FLAT_RESULTS`] values.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct CoreSignature {
    pub params: Vec<Flat>,
    pub results: Vec<Flat>,
    /// The parameters did not fit and are one pointer to them in memory.
    pub params_in_memory: bool,
    /// The result did not fit and is written through the last parameter.
    pub result_in_memory: bool,
}

pub(crate) fn core_signature(
    f: &WitFunc,
    budget: &mut Budget,
) -> Result<CoreSignature, TranspileError> {
    let mut params = Vec::new();
    for (_, t) in &f.params {
        params.extend(flatten(t, budget)?);
    }
    let params_in_memory = params.len() > MAX_FLAT_PARAMS;
    if params_in_memory {
        params = vec![Flat::I32];
    }
    let mut results = match &f.result {
        Some(t) => flatten(t, budget)?,
        None => Vec::new(),
    };
    let result_in_memory = results.len() > MAX_FLAT_RESULTS;
    if result_in_memory {
        results.clear();
        params.push(Flat::I32);
    }
    Ok(CoreSignature {
        params,
        results,
        params_in_memory,
        result_in_memory,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::wit::{WitEnum, WitFlags, WitRecord, WitVariant};

    fn arc(t: WitType) -> Arc<WitType> {
        Arc::new(t)
    }

    fn layout(t: &WitType) -> (u64, u64) {
        size_align(t, &mut Budget::new()).unwrap()
    }

    fn flat(t: &WitType) -> Vec<Flat> {
        flatten(t, &mut Budget::new()).unwrap()
    }

    fn record(fields: Vec<WitType>) -> WitType {
        WitType::Record(WitRecord {
            name: "r".into(),
            fields: fields
                .into_iter()
                .enumerate()
                .map(|(i, t)| (format!("f{i}"), arc(t)))
                .collect(),
        })
    }

    fn variant(cases: Vec<Option<WitType>>) -> WitType {
        WitType::Variant(WitVariant {
            name: "v".into(),
            cases: cases
                .into_iter()
                .enumerate()
                .map(|(i, t)| (format!("c{i}"), t.map(arc)))
                .collect(),
        })
    }

    #[test]
    fn scalar_layouts() {
        assert_eq!(layout(&WitType::U8), (1, 1));
        assert_eq!(layout(&WitType::S16), (2, 2));
        assert_eq!(layout(&WitType::Char), (4, 4));
        assert_eq!(layout(&WitType::F64), (8, 8));
        assert_eq!(layout(&WitType::String), (8, 4));
        assert_eq!(layout(&WitType::List(arc(WitType::U8))), (8, 4));
    }

    #[test]
    fn record_fields_are_aligned_and_the_size_is_padded() {
        // u8 at 0, u32 at 4, u8 at 8: 9 bytes padded to 12.
        let r = record(vec![WitType::U8, WitType::U32, WitType::U8]);
        assert_eq!(layout(&r), (12, 4));
        assert_eq!(layout(&record(vec![])), (0, 1));
    }

    #[test]
    fn variant_layout_places_the_payload_at_its_alignment() {
        // disc u8, payload u64 at 8: 16 bytes, align 8.
        let v = variant(vec![None, Some(WitType::U64)]);
        assert_eq!(layout(&v), (16, 8));
        // option<u8>: disc then the byte.
        assert_eq!(layout(&WitType::Option(arc(WitType::U8))), (2, 1));
        // 257 cases need a u16 discriminant.
        let e = WitType::Enum(WitEnum {
            name: "e".into(),
            cases: (0..257).map(|i| format!("c{i}")).collect(),
        });
        assert_eq!(layout(&e), (2, 2));
    }

    #[test]
    fn flags_layout_by_count() {
        let flags = |n: usize| {
            WitType::Flags(WitFlags {
                name: "f".into(),
                flags: (0..n).map(|i| format!("f{i}")).collect(),
            })
        };
        assert_eq!(layout(&flags(0)), (0, 1));
        assert_eq!(layout(&flags(8)), (1, 1));
        assert_eq!(layout(&flags(9)), (2, 2));
        assert_eq!(layout(&flags(17)), (4, 4));
        assert_eq!(layout(&flags(32)), (4, 4));
        assert_eq!(flat(&flags(0)), vec![]);
        assert_eq!(flat(&flags(32)), vec![Flat::I32]);
    }

    #[test]
    fn scalars_flatten_to_one_core_value() {
        assert_eq!(flat(&WitType::Bool), vec![Flat::I32]);
        assert_eq!(flat(&WitType::S8), vec![Flat::I32]);
        assert_eq!(flat(&WitType::U64), vec![Flat::I64]);
        assert_eq!(flat(&WitType::F32), vec![Flat::F32]);
        assert_eq!(flat(&WitType::String), vec![Flat::I32, Flat::I32]);
    }

    #[test]
    fn a_variant_flattens_to_its_discriminant_and_the_joined_payloads() {
        // f32 vs i32 join to i32; f32 vs i64 join to i64.
        let v = variant(vec![Some(WitType::F32), Some(WitType::U32), None]);
        assert_eq!(flat(&v), vec![Flat::I32, Flat::I32]);
        let v = variant(vec![Some(WitType::F32), Some(WitType::U64)]);
        assert_eq!(flat(&v), vec![Flat::I32, Flat::I64]);
        let v = variant(vec![Some(WitType::F32), Some(WitType::F32)]);
        assert_eq!(flat(&v), vec![Flat::I32, Flat::F32]);
        // The longer payload extends the joined list.
        let pair = record(vec![WitType::F64, WitType::U8]);
        let v = variant(vec![Some(WitType::U8), Some(pair)]);
        assert_eq!(flat(&v), vec![Flat::I32, Flat::I64, Flat::I32]);
    }

    fn func(params: Vec<WitType>, result: Option<WitType>) -> WitFunc {
        WitFunc {
            interface: "a:b/c".into(),
            name: "f".into(),
            params: params
                .into_iter()
                .enumerate()
                .map(|(i, t)| (format!("p{i}"), t))
                .collect(),
            result,
        }
    }

    #[test]
    fn the_core_signature_spills_to_memory_past_the_flat_limits() {
        let sig = |f: &WitFunc| core_signature(f, &mut Budget::new()).unwrap();
        let ok = sig(&func(vec![WitType::U32; 16], Some(WitType::U64)));
        assert_eq!(ok.params.len(), 16);
        assert_eq!(ok.results, vec![Flat::I64]);
        assert!(!ok.params_in_memory && !ok.result_in_memory);

        let spilled = sig(&func(vec![WitType::U32; 17], None));
        assert_eq!(spilled.params, vec![Flat::I32]);
        assert!(spilled.params_in_memory);

        let retptr = sig(&func(vec![WitType::U8], Some(WitType::String)));
        assert_eq!(retptr.params, vec![Flat::I32, Flat::I32]);
        assert!(retptr.results.is_empty() && retptr.result_in_memory);
    }

    #[test]
    fn memory_types_are_found_inside_compounds() {
        let mem = |t: &WitType| needs_memory(t, &mut Budget::new()).unwrap();
        assert!(!mem(&WitType::U32));
        assert!(mem(&WitType::Option(arc(WitType::String))));
        assert!(mem(&record(vec![
            WitType::U8,
            WitType::List(arc(WitType::U8))
        ])));
        assert!(!mem(&variant(vec![None, Some(WitType::U8)])));
    }
}
