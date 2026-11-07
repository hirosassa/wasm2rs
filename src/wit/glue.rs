//! The glue from the typed `Host` trait to the flat reentrant `Imports` trait.
//!
//! `pub struct Glue<H: Host>(pub H)` implements `Imports` (the guest is `Instance<Glue<H>>`,
//! built with `reentrant_imports` and `named_imports`). Each method has the name of the
//! `Host` method it forwards to; it lifts the flat core parameters into the typed ones
//! (canonical ABI `lift_flat`), calls the host, and lowers the typed result back to the flat
//! core result (`lower_flat`).
//!
//! Every flat value is held as its bit pattern in a `u64` (an `i32` zero-extended, an `f32`
//! as its bits, ...). Because the flattened form of a type has a statically known length,
//! lifting and lowering are straight-line code over fixed slot indices; a variant's case
//! payloads all start right after the discriminant, and reading a payload slot as the type
//! of the case looks only at the low 32 bits or all 64, so the spec's coercions from the
//! joined slot type need no code of their own.
//!
//! Only functions that need no linear memory are covered; see [`check`].

use std::fmt::Write as _;

use super::{Records, WitFunc, WitType, case_idents, flags_int, unique_idents, unsupported};
use crate::TranspileError;
use crate::wit_abi::{Budget, CoreSignature, Flat, core_signature, flatten, needs_memory};

/// Helpers the glue calls to validate what the guest passes (a trap is a panic: an
/// `Imports` method has no other way out).
const HELPERS: &str = r#"
mod glue_abi {
    #![allow(dead_code)]

    pub fn trap(msg: &str) -> ! {
        panic!("wasm2rs wit glue: {msg}")
    }

    // The canonical ABI keeps the low bits of a narrow int and takes any non-zero bool as
    // true; only a `char` and a discriminant are checked.
    pub fn bool_(v: u64) -> bool {
        v as u32 != 0
    }

    pub fn char_(v: u64) -> char {
        u32::try_from(v)
            .ok()
            .and_then(char::from_u32)
            .unwrap_or_else(|| trap("invalid char"))
    }

    pub fn u8_(v: u64) -> u8 {
        v as u32 as u8
    }

    pub fn u16_(v: u64) -> u16 {
        v as u32 as u16
    }

    pub fn s8_(v: u64) -> i8 {
        v as u32 as i8
    }

    pub fn s16_(v: u64) -> i16 {
        v as u32 as i16
    }

    pub fn disc(v: u64, cases: u32) -> u32 {
        match u32::try_from(v) {
            Ok(d) if d < cases => d,
            _ => trap("invalid discriminant"),
        }
    }

    // Only the low `count` bits are flags; the others are ignored.
    pub fn flags(v: u64, count: u32) -> u32 {
        let bits = v as u32;
        if count >= 32 { bits } else { bits & ((1 << count) - 1) }
    }
}
"#;

/// The error for a function the glue does not cover.
fn not_yet(f: &WitFunc, reason: &str) -> TranspileError {
    unsupported(format!(
        "glue for `{}` of `{}`: {reason} (not yet)",
        f.name, f.interface
    ))
}

/// Checks that glue can be generated for `f`: only functions made of scalars, enums, flags,
/// resource handles and flat variants, options, results, records and tuples with at most
/// [`MAX_FLAT_PARAMS`](crate::wit_abi::MAX_FLAT_PARAMS) flat parameters and a result that
/// flattens to at most one core value. A `string` or `list` (a pointer into linear memory),
/// too many flat parameters (a pointer to them) or a larger flat result (a return pointer)
/// need memory and are refused.
fn check(f: &WitFunc, budget: &mut Budget) -> Result<CoreSignature, TranspileError> {
    for (name, ty) in &f.params {
        if needs_memory(ty, budget)? {
            return Err(not_yet(
                f,
                &format!("parameter `{name}` holds a string or list, which needs linear memory"),
            ));
        }
    }
    if let Some(ty) = &f.result
        && needs_memory(ty, budget)?
    {
        return Err(not_yet(
            f,
            "the result holds a string or list, which needs linear memory",
        ));
    }
    let sig = core_signature(f, budget)?;
    if sig.params_in_memory {
        return Err(not_yet(
            f,
            "the parameters flatten to more than 16 core values and are passed through memory",
        ));
    }
    if sig.result_in_memory {
        return Err(not_yet(
            f,
            "the result flattens to more than 1 core value and is returned through memory",
        ));
    }
    Ok(sig)
}

fn core_rust(flat: Flat) -> &'static str {
    match flat {
        Flat::I32 => "i32",
        Flat::I64 => "i64",
        Flat::F32 => "f32",
        Flat::F64 => "f64",
    }
}

/// The `u64` bit pattern of the core value `name`.
fn to_slot(flat: Flat, name: &str) -> String {
    match flat {
        Flat::I32 => format!("{name} as u32 as u64"),
        Flat::I64 => format!("{name} as u64"),
        Flat::F32 => format!("u64::from({name}.to_bits())"),
        Flat::F64 => format!("{name}.to_bits()"),
    }
}

/// The core value of type `flat` held in the slot expression `slot`.
fn from_slot(flat: Flat, slot: &str) -> String {
    match flat {
        Flat::I32 => format!("{slot} as u32 as i32"),
        Flat::I64 => format!("{slot} as i64"),
        Flat::F32 => format!("f32::from_bits({slot} as u32)"),
        Flat::F64 => format!("f64::from_bits({slot})"),
    }
}

/// A variant-like type seen as discriminated cases: how to build or match each case in Rust
/// and its payload type.
struct Case {
    /// The constructor or pattern head, e.g. `Some`, `Ok`, `Shape::Circle`.
    path: String,
    payload: Option<WitType>,
    /// A case without payload that Rust still writes with `(())` (`Ok(())`).
    unit_arg: bool,
}

impl Case {
    fn new(path: impl Into<String>, payload: Option<&WitType>) -> Self {
        Case {
            path: path.into(),
            payload: payload.cloned(),
            unit_arg: false,
        }
    }

    /// The Rust expression building the case from the Rust expression `payload`.
    fn build(&self, payload: Option<String>) -> String {
        match (payload, self.unit_arg) {
            (Some(p), _) => format!("{}({p})", self.path),
            (None, true) => format!("{}(())", self.path),
            (None, false) => self.path.clone(),
        }
    }

    /// The Rust pattern matching the case, binding the payload to `x`.
    fn pattern(&self) -> String {
        match (&self.payload, self.unit_arg) {
            (Some(_), _) => format!("{}(x)", self.path),
            (None, true) => format!("{}(())", self.path),
            (None, false) => self.path.clone(),
        }
    }
}

struct Gen<'a> {
    records: &'a Records,
    budget: Budget,
}

impl Gen<'_> {
    fn flat_len(&mut self, ty: &WitType) -> Result<usize, TranspileError> {
        Ok(flatten(ty, &mut self.budget)?.len())
    }

    /// The cases of an option, result, variant or enum; `None` for any other type.
    fn cases(&self, ty: &WitType) -> Result<Option<Vec<Case>>, TranspileError> {
        Ok(Some(match ty {
            WitType::Option(t) => vec![Case::new("None", None), Case::new("Some", Some(t))],
            WitType::Result { ok, err } => [("Ok", ok), ("Err", err)]
                .into_iter()
                .map(|(path, t)| Case {
                    path: path.to_string(),
                    payload: t.as_deref().cloned(),
                    unit_arg: t.is_none(),
                })
                .collect(),
            WitType::Variant(v) => {
                let name = self.records.rust(ty)?;
                case_idents(v.cases.iter().map(|(n, _)| n.as_str()))
                    .into_iter()
                    .zip(&v.cases)
                    .map(|(ident, (_, t))| Case::new(format!("{name}::{ident}"), t.as_deref()))
                    .collect()
            }
            WitType::Enum(e) => {
                let name = self.records.rust(ty)?;
                case_idents(e.cases.iter().map(String::as_str))
                    .into_iter()
                    .map(|ident| Case::new(format!("{name}::{ident}"), None))
                    .collect()
            }
            _ => return Ok(None),
        }))
    }

    /// A Rust expression lifting the value of `ty` held in slots `s[base..]`.
    fn lift(&mut self, ty: &WitType, base: usize) -> Result<String, TranspileError> {
        self.budget.spend()?;
        let s = format!("s[{base}]");
        if let Some(cases) = self.cases(ty)? {
            return self.lift_cases(&cases, base);
        }
        Ok(match ty {
            WitType::U8 => format!("glue_abi::u8_({s})"),
            WitType::U16 => format!("glue_abi::u16_({s})"),
            WitType::U32 => format!("{s} as u32"),
            WitType::U64 => s,
            WitType::S8 => format!("glue_abi::s8_({s})"),
            WitType::S16 => format!("glue_abi::s16_({s})"),
            WitType::S32 => format!("{s} as u32 as i32"),
            WitType::S64 => format!("{s} as i64"),
            WitType::F32 => format!("f32::from_bits({s} as u32)"),
            WitType::F64 => format!("f64::from_bits({s})"),
            WitType::Bool => format!("glue_abi::bool_({s})"),
            WitType::Char => format!("glue_abi::char_({s})"),
            WitType::Own(_) | WitType::Borrow(_) => {
                format!("{}({s} as u32)", self.records.rust(ty)?)
            }
            WitType::Flags(f) => {
                let name = self.records.rust(ty)?;
                let count = f.flags.len();
                if count == 0 {
                    format!("{name}(0)")
                } else {
                    let int = flags_int(f);
                    format!("{name}(glue_abi::flags({s}, {count}) as {int})")
                }
            }
            WitType::Tuple(ts) => {
                let mut offset = base;
                let mut parts = Vec::new();
                for t in ts {
                    parts.push(self.lift(t, offset)?);
                    offset = offset.saturating_add(self.flat_len(t)?);
                }
                match parts.as_slice() {
                    [one] => format!("({one},)"),
                    many => format!("({})", many.join(", ")),
                }
            }
            WitType::Record(r) => {
                let name = self.records.rust(ty)?;
                let idents = unique_idents(r.fields.iter().map(|(n, _)| n.as_str()));
                let mut offset = base;
                let mut fields = String::new();
                for (ident, (_, t)) in idents.iter().zip(&r.fields) {
                    let _ = write!(fields, "{ident}: {}, ", self.lift(t, offset)?);
                    offset = offset.saturating_add(self.flat_len(t)?);
                }
                format!("{name} {{ {fields}}}")
            }
            // `String` and `List` never get here: `check` refuses them.
            _ => {
                return Err(unsupported(
                    "glue: a string or list reached the flat lift".to_string(),
                ));
            }
        })
    }

    fn lift_cases(&mut self, cases: &[Case], base: usize) -> Result<String, TranspileError> {
        let mut arms = String::new();
        for (i, case) in cases.iter().enumerate() {
            let payload = match &case.payload {
                Some(t) => Some(self.lift(t, base.saturating_add(1))?),
                None => None,
            };
            let _ = write!(arms, "{i} => {}, ", case.build(payload));
        }
        Ok(format!(
            "match glue_abi::disc(s[{base}], {}) {{ {arms}_ => glue_abi::trap(\"invalid discriminant\") }}",
            cases.len()
        ))
    }

    /// Statements storing the flat form of the Rust expression `value` of type `ty` into
    /// `out[base..]` (which starts zeroed, so the slots a variant case leaves unused stay 0).
    fn lower(&mut self, ty: &WitType, value: &str, base: usize) -> Result<String, TranspileError> {
        self.budget.spend()?;
        if matches!(ty, WitType::Enum(_)) {
            return Ok(format!("out[{base}] = ({value}) as u32 as u64;"));
        }
        if let Some(cases) = self.cases(ty)? {
            let mut arms = String::new();
            for (i, case) in cases.iter().enumerate() {
                let body = match &case.payload {
                    Some(t) => self.lower(t, "x", base.saturating_add(1))?,
                    None => String::new(),
                };
                let _ = write!(
                    arms,
                    "{} => {{ out[{base}] = {i}; {body} }} ",
                    case.pattern()
                );
            }
            return Ok(format!("match {value} {{ {arms}}}"));
        }
        Ok(match ty {
            WitType::U8 | WitType::U16 | WitType::U32 => {
                format!("out[{base}] = u64::from({value});")
            }
            WitType::U64 => format!("out[{base}] = {value};"),
            WitType::S8 | WitType::S16 | WitType::S32 => {
                format!("out[{base}] = ({value}) as i32 as u32 as u64;")
            }
            WitType::S64 => format!("out[{base}] = ({value}) as u64;"),
            WitType::F32 => format!("out[{base}] = u64::from(({value}).to_bits());"),
            WitType::F64 => format!("out[{base}] = ({value}).to_bits();"),
            WitType::Bool => format!("out[{base}] = u64::from({value});"),
            WitType::Char => format!("out[{base}] = u64::from(u32::from({value}));"),
            WitType::Own(_) | WitType::Borrow(_) => {
                format!("out[{base}] = u64::from(({value}).0);")
            }
            WitType::Flags(f) => {
                if f.flags.is_empty() {
                    // No core value: nothing to store (and `value` is not read).
                    String::new()
                } else {
                    format!("out[{base}] = u64::from(({value}).0);")
                }
            }
            WitType::Tuple(ts) => {
                let mut offset = base;
                let mut body = String::new();
                for (i, t) in ts.iter().enumerate() {
                    let _ = write!(body, "{} ", self.lower(t, &format!("v.{i}"), offset)?);
                    offset = offset.saturating_add(self.flat_len(t)?);
                }
                format!("{{ let v = {value}; {body}}}")
            }
            WitType::Record(r) => {
                let idents = unique_idents(r.fields.iter().map(|(n, _)| n.as_str()));
                let mut offset = base;
                let mut body = String::new();
                for (ident, (_, t)) in idents.iter().zip(&r.fields) {
                    let _ = write!(body, "{} ", self.lower(t, &format!("v.{ident}"), offset)?);
                    offset = offset.saturating_add(self.flat_len(t)?);
                }
                format!("{{ let v = {value}; {body}}}")
            }
            _ => {
                return Err(unsupported(
                    "glue: a string or list reached the flat lower".to_string(),
                ));
            }
        })
    }
}

/// Appends the glue to `out`: the helpers, `Glue` and its `Imports` impl, one method per
/// function of `funcs` (named `names[i]`, like the `Host` method).
///
/// # Errors
///
/// A function [`check`] refuses, naming it and the reason.
pub(super) fn write(
    out: &mut String,
    funcs: &[WitFunc],
    names: &[String],
    records: &Records,
) -> Result<(), TranspileError> {
    let mut budget = Budget::new();
    let mut sigs = Vec::new();
    for f in funcs {
        sigs.push(check(f, &mut budget)?);
    }
    let mut gen_ = Gen {
        records,
        budget: Budget::new(),
    };
    out.push_str(HELPERS);
    out.push_str(
        "\n/// Lowers a typed `Host` to the flat `Imports` of the core module (transpiled with\n\
         /// `named_imports` and `reentrant_imports`): `Instance<Glue<H>>`.\n\
         pub struct Glue<H: Host>(pub H);\n\n\
         #[allow(unused_variables)]\n\
         impl<H: Host> Imports for Glue<H> {\n",
    );
    for ((f, name), sig) in funcs.iter().zip(names).zip(&sigs) {
        let _ = writeln!(out, "    /// {:?} {:?}", f.interface, f.name);
        let mut head = format!("    fn {name}(&mut self, _g: &mut Instance<Self>");
        for (i, flat) in sig.params.iter().enumerate() {
            let _ = write!(head, ", p{i}: {}", core_rust(*flat));
        }
        head.push(')');
        if let Some(flat) = sig.results.first() {
            let _ = write!(head, " -> {}", core_rust(*flat));
        }
        let _ = writeln!(out, "{head} {{");
        if !sig.params.is_empty() {
            let slots: Vec<String> = sig
                .params
                .iter()
                .enumerate()
                .map(|(i, flat)| to_slot(*flat, &format!("p{i}")))
                .collect();
            let _ = writeln!(out, "        let s = [{}];", slots.join(", "));
        }
        let mut args = Vec::new();
        let mut offset = 0usize;
        for (_, ty) in &f.params {
            args.push(gen_.lift(ty, offset)?);
            offset = offset.saturating_add(gen_.flat_len(ty)?);
        }
        let call = format!("self.0.{name}({})", args.join(", "));
        match (&f.result, sig.results.first()) {
            (Some(ty), Some(flat)) => {
                let _ = writeln!(out, "        let r = {call};");
                let _ = writeln!(out, "        let mut out = [0u64; 1];");
                let _ = writeln!(out, "        {}", gen_.lower(ty, "r", 0)?);
                let _ = writeln!(out, "        {}", from_slot(*flat, "out[0]"));
            }
            // A result with no core value (an empty record or flags): call and drop it.
            (Some(_), None) => {
                let _ = writeln!(out, "        let _ = {call};");
            }
            (None, _) => {
                let _ = writeln!(out, "        {call};");
            }
        }
        out.push_str("    }\n");
    }
    out.push_str("}\n");
    Ok(())
}
