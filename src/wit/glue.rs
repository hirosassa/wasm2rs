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
//! Memory-backed types are covered too. A `string` or `list` is a `(ptr, len)` pair into the
//! guest's linear memory, read through `g.memory()`; parameters that flatten to more than 16
//! core values arrive as one pointer to a record of them; a result of more than one core value
//! is written through a trailing return pointer. Lowering a `string` or `list` calls the
//! guest's `cabi_realloc(0, 0, align, size)` (the core module must export it, so transpile
//! with `named_exports`). Everything the guest hands over is checked like the canonical ABI
//! says: an out-of-range or unaligned pointer, invalid UTF-8, a bad discriminant or `char` is a
//! trap, which here is a panic.

use std::fmt::Write as _;

use super::emit::{Records, case_idents, doc_text, flags_int, typed_signature, unique_idents};
use super::read::unsupported;
use super::{WitExport, WitFunc, WitType};
use crate::TranspileError;
use crate::codegen::assign_export_names;
use crate::wit_abi::{
    Budget, Flat, core_signature, field_offsets, flatten, needs_memory, record_layout, size_align,
    variant_layout,
};

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

    /// What one call may still lift (copy out of guest memory): the guest's memory length at
    /// the start of the call. Every lifted string or list spends its byte length and its
    /// element count, so descriptors that alias the same bytes, or a huge list of zero-sized
    /// elements, cannot make the host copy or loop more than memory could hold.
    pub struct Budget(std::cell::Cell<u64>);

    impl Budget {
        pub fn new(mem_len: usize) -> Self {
            Budget(std::cell::Cell::new(mem_len as u64))
        }

        pub fn spend(&self, n: u64) {
            match self.0.get().checked_sub(n) {
                Some(left) => self.0.set(left),
                None => trap("lifted data too large"),
            }
        }
    }

    /// The `len` bytes of guest memory at `at`.
    pub fn slice(m: &[u8], at: u64, len: u64) -> &[u8] {
        let range = at
            .checked_add(len)
            .and_then(|end| Some((usize::try_from(at).ok()?, usize::try_from(end).ok()?)))
            .and_then(|(a, e)| m.get(a..e));
        range.unwrap_or_else(|| trap("pointer out of bounds"))
    }

    /// The little-endian unsigned integer of `n` bytes at `at`.
    pub fn ld(m: &[u8], at: u64, n: usize) -> u64 {
        let mut bytes = [0u8; 8];
        bytes[..n].copy_from_slice(slice(m, at, n as u64));
        u64::from_le_bytes(bytes)
    }

    /// A pointer is a multiple of `align` and `size` bytes from it are in memory.
    pub fn check(mem_len: usize, at: u64, size: u64, align: u64) {
        if at % align.max(1) != 0 {
            trap("unaligned pointer");
        }
        match at.checked_add(size) {
            Some(end) if end <= mem_len as u64 => {}
            _ => trap("pointer out of bounds"),
        }
    }

    /// Checks a list of `len` elements of `size` bytes and spends its elements and bytes.
    pub fn check_list(m: &[u8], lb: &Budget, ptr: u32, len: u32, size: u64, align: u64) {
        let bytes = u64::from(len) * size;
        lb.spend(bytes.saturating_add(u64::from(len)));
        check(m.len(), u64::from(ptr), bytes, align);
    }

    /// The bytes of a `string` or `list<u8>`: the length is both its bytes and its elements.
    pub fn bytes(m: &[u8], lb: &Budget, ptr: u32, len: u32) -> Vec<u8> {
        lb.spend(u64::from(len));
        slice(m, u64::from(ptr), u64::from(len)).to_vec()
    }

    pub fn string(m: &[u8], lb: &Budget, ptr: u32, len: u32) -> String {
        String::from_utf8(bytes(m, lb, ptr, len)).unwrap_or_else(|_| trap("invalid UTF-8 in a string"))
    }
}
"#;

/// Writes into guest memory; present only when a result is returned through memory.
const MEM_HELPERS: &str = r#"
mod glue_mem {
    #![allow(dead_code, unused_imports)]
    use super::*;

    pub fn st<H: Host>(g: &mut Instance<Glue<H>>, at: u64, bytes: &[u8]) {
        let m = g.memory();
        let len = m.len() as u64;
        match at.checked_add(bytes.len() as u64) {
            Some(end) if end <= len => m[at as usize..end as usize].copy_from_slice(bytes),
            _ => glue_abi::trap("pointer out of bounds"),
        }
    }
}
"#;

/// The module of [`ALLOC_HELPERS`], which allocates through `cabi_realloc`.
const ALLOC_MODULE: &str = "glue_alloc";

/// The module of [`ALLOC_HELPERS`] with the first import `realloc` in place of
/// `cabi_realloc`; the n-th (n > 0) is `{IMPORT_ALLOC_MODULE}_{n}`.
const IMPORT_ALLOC_MODULE: &str = "glue_alloc_import";

fn import_alloc_module(n: usize) -> String {
    match n {
        0 => IMPORT_ALLOC_MODULE.to_string(),
        n => format!("{IMPORT_ALLOC_MODULE}_{n}"),
    }
}

/// Allocating in the guest; present only when a `string` or `list` is returned.
const ALLOC_HELPERS: &str = r#"
mod glue_alloc {
    #![allow(dead_code, unused_imports)]
    use super::*;

    /// `count * size` bytes from the guest's `cabi_realloc`, aligned to `align`.
    pub fn alloc<H: Host>(g: &mut Instance<Glue<H>>, align: u64, count: u64, size: u64) -> u64 {
        let bytes = match count.checked_mul(size) {
            Some(b) if b <= u64::from(u32::MAX) => b,
            _ => glue_abi::trap("list too large"),
        };
        let ptr = u64::from(g.cabi_realloc(0, 0, align as i32, bytes as i32) as u32);
        glue_abi::check(g.memory().len(), ptr, bytes, align);
        ptr
    }

    /// Copies `data` into a fresh guest allocation and writes its `(ptr, len)` at `at`.
    pub fn put_bytes<H: Host>(g: &mut Instance<Glue<H>>, at: u64, data: &[u8]) {
        let ptr = alloc(g, 1, data.len() as u64, 1);
        glue_mem::st(g, ptr, data);
        put_ptr_len(g, at, ptr, data.len() as u64);
    }

    /// Writes the `(ptr, len)` of a string or list at `at`.
    pub fn put_ptr_len<H: Host>(g: &mut Instance<Glue<H>>, at: u64, ptr: u64, len: u64) {
        glue_mem::st(g, at, &(ptr as u32).to_le_bytes());
        glue_mem::st(g, at + 4, &(len as u32).to_le_bytes());
    }
}
"#;

/// [`ALLOC_HELPERS`] calling the guest's `realloc` export instead of `cabi_realloc`, in the
/// module `module`.
fn import_alloc_helpers(realloc: &str, module: &str) -> String {
    ALLOC_HELPERS
        .replace(&format!("mod {ALLOC_MODULE} "), &format!("mod {module} "))
        .replace("g.cabi_realloc(", &format!("g.{realloc}("))
        .replace("`cabi_realloc`", &format!("`{realloc}`"))
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

/// Where a value is lifted from: the flat core parameters (`s[n..]`) or memory at an address
/// (a Rust `u64` expression).
enum Src {
    Flat(usize),
    Mem(String),
}

impl Src {
    /// The source `off` further on: `off` more flat slots, or `off` more bytes.
    fn advance(&self, off: u64) -> Src {
        match self {
            Src::Flat(b) => Src::Flat(b.saturating_add(usize::try_from(off).unwrap_or(usize::MAX))),
            Src::Mem(at) => Src::Mem(offset_expr(at, off)),
        }
    }
}

/// The bytes a scalar (or flags, or a discriminant) takes in memory.
fn mem_width(ty: &WitType) -> usize {
    match ty {
        WitType::U8 | WitType::S8 | WitType::Bool => 1,
        WitType::U16 | WitType::S16 => 2,
        WitType::U64 | WitType::S64 | WitType::F64 => 8,
        // A flags type without flags has no value to load, so its width is never used.
        WitType::Flags(f) => match f.flags.len() {
            1..=8 => 1,
            9..=16 => 2,
            _ => 4,
        },
        _ => 4,
    }
}

struct Gen<'a> {
    records: &'a Records,
    budget: Budget,
    /// A counter for the variable names of nested lists, records and the like.
    fresh: usize,
    /// Some result is written through a pointer.
    writes_memory: bool,
    /// Some result is a `string` or `list`, allocated through `cabi_realloc`.
    allocates: bool,
    /// The module of allocation helpers the generated code calls (see [`ALLOC_MODULE`]).
    alloc: String,
}

impl Gen<'_> {
    fn flat_len(&mut self, ty: &WitType) -> Result<usize, TranspileError> {
        Ok(flatten(ty, &mut self.budget)?.len())
    }

    fn fresh(&mut self) -> usize {
        self.fresh = self.fresh.saturating_add(1);
        self.fresh
    }

    /// The `(size, align)` of `ty` in memory.
    fn layout(&mut self, ty: &WitType) -> Result<(u64, u64), TranspileError> {
        size_align(ty, &mut self.budget)
    }

    /// The discriminant size and payload offset of a variant-like type. Every caller has
    /// matched `cases` first, so the `(1, 1)` fallback for any other type is never used.
    fn variant_layout(&mut self, ty: &WitType) -> Result<(u64, u64), TranspileError> {
        Ok(variant_layout(ty, &mut self.budget)?.unwrap_or((1, 1)))
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

    /// The sources of consecutive fields of `tys` laid out from `src`.
    fn field_srcs(&mut self, tys: &[&WitType], src: &Src) -> Result<Vec<Src>, TranspileError> {
        match src {
            Src::Flat(base) => {
                let mut offset = *base;
                let mut out = Vec::new();
                for t in tys {
                    out.push(Src::Flat(offset));
                    offset = offset.saturating_add(self.flat_len(t)?);
                }
                Ok(out)
            }
            Src::Mem(at) => Ok(field_offsets(tys, &mut self.budget)?
                .into_iter()
                .map(|off| Src::Mem(offset_expr(at, off)))
                .collect()),
        }
    }

    /// The `u64` bit pattern of a `width`-byte value at `src`.
    fn raw(src: &Src, width: usize) -> String {
        match src {
            Src::Flat(base) => format!("s[{base}]"),
            Src::Mem(at) => format!("glue_abi::ld(m, {at}, {width})"),
        }
    }

    /// A Rust expression lifting the value of `ty` at `src`.
    fn lift(&mut self, ty: &WitType, src: &Src) -> Result<String, TranspileError> {
        self.budget.spend()?;
        if let Some(cases) = self.cases(ty)? {
            return self.lift_cases(ty, &cases, src);
        }
        let s = Self::raw(src, mem_width(ty));
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
                let tys: Vec<&WitType> = ts.iter().map(AsRef::as_ref).collect();
                let srcs = self.field_srcs(&tys, src)?;
                let mut parts = Vec::new();
                for (t, src) in tys.iter().zip(&srcs) {
                    parts.push(self.lift(t, src)?);
                }
                match parts.as_slice() {
                    [one] => format!("({one},)"),
                    many => format!("({})", many.join(", ")),
                }
            }
            WitType::Record(r) => {
                let name = self.records.rust(ty)?;
                let idents = unique_idents(r.fields.iter().map(|(n, _)| n.as_str()));
                let tys: Vec<&WitType> = r.fields.iter().map(|(_, t)| t.as_ref()).collect();
                let srcs = self.field_srcs(&tys, src)?;
                let mut fields = String::new();
                for ((ident, t), src) in idents.iter().zip(&tys).zip(&srcs) {
                    let _ = write!(fields, "{ident}: {}, ", self.lift(t, src)?);
                }
                format!("{name} {{ {fields}}}")
            }
            WitType::String => {
                let (ptr, len) = Self::ptr_len(src);
                format!("glue_abi::string(m, &lb, {ptr}, {len})")
            }
            WitType::List(t) => {
                let (ptr, len) = Self::ptr_len(src);
                if matches!(**t, WitType::U8) {
                    return Ok(format!("glue_abi::bytes(m, &lb, {ptr}, {len})"));
                }
                let (size, align) = self.layout(t)?;
                let id = self.fresh();
                let elem = self.lift(
                    t,
                    &Src::Mem(format!("(u64::from(p{id}) + u64::from(i{id}) * {size})")),
                )?;
                format!(
                    "{{ let p{id} = {ptr}; let n{id} = {len}; \
                     glue_abi::check_list(m, &lb, p{id}, n{id}, {size}, {align}); \
                     (0..n{id}).map(|i{id}| {elem}).collect::<Vec<_>>() }}"
                )
            }
            // Variant-like types were lifted above.
            WitType::Option(_)
            | WitType::Result { .. }
            | WitType::Variant(_)
            | WitType::Enum(_) => {
                return Err(unsupported(
                    "glue: a variant-like type reached the scalar lift".to_string(),
                ));
            }
        })
    }

    /// The `(ptr, len)` `u32` expressions of the string or list at `src`.
    fn ptr_len(src: &Src) -> (String, String) {
        let word = |src: &Src| format!("{} as u32", Self::raw(src, 4));
        let len_at = match src {
            Src::Flat(_) => 1,
            Src::Mem(_) => 4,
        };
        (word(src), word(&src.advance(len_at)))
    }

    fn lift_cases(
        &mut self,
        ty: &WitType,
        cases: &[Case],
        src: &Src,
    ) -> Result<String, TranspileError> {
        let (disc_size, payload_off) = match src {
            Src::Flat(_) => (4, 1),
            Src::Mem(_) => self.variant_layout(ty)?,
        };
        let disc = Self::raw(src, usize::try_from(disc_size).unwrap_or(4));
        let payload_src = src.advance(payload_off);
        let mut arms = String::new();
        for (i, case) in cases.iter().enumerate() {
            let payload = match &case.payload {
                Some(t) => Some(self.lift(t, &payload_src)?),
                None => None,
            };
            let _ = write!(arms, "{i} => {}, ", case.build(payload));
        }
        Ok(format!(
            "match glue_abi::disc({disc}, {}) {{ {arms}_ => glue_abi::trap(\"invalid discriminant\") }}",
            cases.len()
        ))
    }

    /// Statements storing the flat form of the Rust expression `value` of type `ty` into
    /// `out[base..]` (which starts zeroed, so the slots a variant case leaves unused stay 0).
    /// A `string` or `list` is allocated in the guest, so `g` must be in scope.
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
            // Only a lowered parameter of an export gets here: an import's result that holds a
            // `string` or `list` is returned through memory (`store`).
            WitType::String => {
                self.allocates = true;
                self.writes_memory = true;
                let next = base.saturating_add(1);
                let al = self.alloc.clone();
                format!(
                    "{{ let v = {value}; let b = v.as_bytes(); \
                     let p = {al}::alloc(g, 1, b.len() as u64, 1); glue_mem::st(g, p, b); \
                     out[{base}] = p; out[{next}] = b.len() as u64; }}"
                )
            }
            WitType::List(t) => {
                self.allocates = true;
                self.writes_memory = true;
                let next = base.saturating_add(1);
                let al = self.alloc.clone();
                if matches!(**t, WitType::U8) {
                    return Ok(format!(
                        "{{ let v = {value}; let p = {al}::alloc(g, 1, v.len() as u64, 1); \
                         glue_mem::st(g, p, &v); out[{base}] = p; out[{next}] = v.len() as u64; }}"
                    ));
                }
                let (size, align) = self.layout(t)?;
                let id = self.fresh();
                let elem_at = format!("(p{id} + (i{id} as u64) * {size})");
                let elem = self.store(t, &format!("x{id}"), &elem_at)?;
                format!(
                    "{{ let v{id} = {value}; let n{id} = v{id}.len() as u64; \
                     let p{id} = {al}::alloc(g, {align}, n{id}, {size}); \
                     for (i{id}, x{id}) in v{id}.into_iter().enumerate() {{ {elem} }} \
                     out[{base}] = p{id}; out[{next}] = n{id}; }}"
                )
            }
            _ => String::new(),
        })
    }

    /// Statements storing the Rust expression `value` of type `ty` into guest memory at the
    /// address expression `at` (`glue_mem::st`), allocating in the guest for a `string` or
    /// `list`. Padding is left as it is.
    fn store(&mut self, ty: &WitType, value: &str, at: &str) -> Result<String, TranspileError> {
        self.budget.spend()?;
        self.writes_memory = true;
        let put = |bytes: String| format!("glue_mem::st(g, {at}, &{bytes});");
        if let Some(cases) = self.cases(ty)? {
            let (disc_size, payload_off) = self.variant_layout(ty)?;
            let payload_at = offset_expr(at, payload_off);
            let disc = |i: String| put(format!("({i}).to_le_bytes()[..{disc_size}]"));
            if matches!(ty, WitType::Enum(_)) {
                return Ok(disc(format!("({value}) as u32")));
            }
            let mut arms = String::new();
            for (i, case) in cases.iter().enumerate() {
                let body = match &case.payload {
                    Some(t) => self.store(t, "x", &payload_at)?,
                    None => String::new(),
                };
                let d = disc(format!("{i}u32"));
                let _ = write!(arms, "{} => {{ {d} {body} }} ", case.pattern());
            }
            return Ok(format!("match {value} {{ {arms}}}"));
        }
        Ok(match ty {
            WitType::U8 => put(format!("[{value}]")),
            WitType::S8 => put(format!("[({value}) as u8]")),
            WitType::Bool => put(format!("[u8::from({value})]")),
            WitType::U16
            | WitType::U32
            | WitType::U64
            | WitType::S16
            | WitType::S32
            | WitType::S64
            | WitType::F32
            | WitType::F64 => put(format!("({value}).to_le_bytes()")),
            WitType::Char => put(format!("u32::from({value}).to_le_bytes()")),
            WitType::Own(_) | WitType::Borrow(_) => put(format!("({value}).0.to_le_bytes()")),
            WitType::Flags(f) => {
                if f.flags.is_empty() {
                    String::new()
                } else {
                    put(format!("({value}).0.to_le_bytes()"))
                }
            }
            WitType::Tuple(ts) => {
                let tys: Vec<&WitType> = ts.iter().map(AsRef::as_ref).collect();
                let names: Vec<String> = (0..tys.len()).map(|i| i.to_string()).collect();
                self.store_fields(&tys, &names, value, at)?
            }
            WitType::Record(r) => {
                let tys: Vec<&WitType> = r.fields.iter().map(|(_, t)| t.as_ref()).collect();
                let names = unique_idents(r.fields.iter().map(|(n, _)| n.as_str()));
                self.store_fields(&tys, &names, value, at)?
            }
            WitType::String => {
                self.allocates = true;
                let al = self.alloc.clone();
                format!("{al}::put_bytes(g, {at}, ({value}).as_bytes());")
            }
            WitType::List(t) => {
                self.allocates = true;
                let al = self.alloc.clone();
                if matches!(**t, WitType::U8) {
                    return Ok(format!("{al}::put_bytes(g, {at}, &({value}));"));
                }
                let (size, align) = self.layout(t)?;
                let id = self.fresh();
                let elem_at = format!("(p{id} + (i{id} as u64) * {size})");
                let elem = self.store(t, &format!("x{id}"), &elem_at)?;
                format!(
                    "{{ let v{id} = {value}; let n{id} = v{id}.len() as u64; \
                     let p{id} = {al}::alloc(g, {align}, n{id}, {size}); \
                     for (i{id}, x{id}) in v{id}.into_iter().enumerate() {{ {elem} }} \
                     {al}::put_ptr_len(g, {at}, p{id}, n{id}); }}"
                )
            }
            _ => String::new(),
        })
    }
}

impl Gen<'_> {
    /// Stores the fields `value.<names[i]>` of a tuple or record, each at its offset from `at`.
    fn store_fields(
        &mut self,
        tys: &[&WitType],
        names: &[String],
        value: &str,
        at: &str,
    ) -> Result<String, TranspileError> {
        let offsets = field_offsets(tys, &mut self.budget)?;
        let id = self.fresh();
        let mut body = String::new();
        for ((t, name), off) in tys.iter().zip(names).zip(offsets) {
            let _ = write!(
                body,
                "{} ",
                self.store(t, &format!("v{id}.{name}"), &offset_expr(at, off))?
            );
        }
        Ok(format!("{{ let v{id} = {value}; {body}}}"))
    }
}

/// The address expression `at + offset`.
fn offset_expr(at: &str, offset: u64) -> String {
    if offset == 0 {
        at.to_string()
    } else {
        format!("({at} + {offset})")
    }
}

/// A method of the glue's `Imports` impl: `name` is the core module's, which lowers the WIT
/// function `func` by calling the `Host` method `host`.
pub(super) struct Method<'a> {
    pub func: &'a WitFunc,
    pub name: &'a str,
    pub host: &'a str,
    /// The export of the program that this import allocates its string and list results
    /// through, or `None` for the lifts' `cabi_realloc`.
    pub realloc: Option<&'a str>,
}

/// Appends the glue to `out`: the helpers, `Glue` and its `Imports` impl with one method per
/// entry of `methods`, and with `exports` the `Exports` trait and its impl (see
/// [`write_exports`]).
///
/// # Errors
///
/// A type that expands past the node budget.
pub(super) fn write(
    out: &mut String,
    methods: &[Method<'_>],
    records: &Records,
    exports: Option<(&[WitExport], &[String])>,
) -> Result<(), TranspileError> {
    let mut sig_budget = Budget::new();
    let mut gen_ = Gen {
        records,
        budget: Budget::new(),
        fresh: 0,
        writes_memory: false,
        allocates: false,
        alloc: ALLOC_MODULE.to_string(),
    };
    // The distinct import `realloc`s in first-use order, and whether an import allocates
    // through each (the last entry: through the lifts' `cabi_realloc`).
    let mut reallocs: Vec<(&str, bool)> = Vec::new();
    let mut default_allocates = false;
    out.push_str(HELPERS);
    out.push_str(
        "\n/// Lowers a typed `Host` to the flat `Imports` of the core module (transpiled with\n\
         /// `named_imports`, `reentrant_imports` and `named_exports`): `Instance<Glue<H>>`.\n\
         pub struct Glue<H: Host>(pub H);\n\n\
         #[allow(unused_variables, unused_parens)]\n\
         impl<H: Host> Imports for Glue<H> {\n",
    );
    for &Method {
        func: f,
        name,
        host,
        realloc,
    } in methods
    {
        let slot = realloc.map(|r| match reallocs.iter().position(|(n, _)| *n == r) {
            Some(at) => at,
            None => {
                reallocs.push((r, false));
                reallocs.len().saturating_sub(1)
            }
        });
        gen_.alloc = slot.map_or_else(|| ALLOC_MODULE.to_string(), import_alloc_module);
        let sig = core_signature(f, &mut sig_budget)?;
        let _ = writeln!(out, "    /// {:?} {:?}", f.interface, f.name);
        let mut head = format!("    fn {name}(&mut self, g: &mut Instance<Self>");
        for (i, flat) in sig.params.iter().enumerate() {
            let _ = write!(head, ", p{i}: {}", core_rust(*flat));
        }
        head.push(')');
        if let Some(flat) = sig.results.first() {
            let _ = write!(head, " -> {}", core_rust(*flat));
        }
        let _ = writeln!(out, "{head} {{");

        // The parameters: flat in `s`, or one pointer to a record of them.
        let mut reads_memory = sig.params_in_memory;
        for (_, ty) in &f.params {
            reads_memory |= needs_memory(ty, &mut sig_budget)?;
        }
        let mut prologue = String::new();
        let tys: Vec<&WitType> = f.params.iter().map(|(_, t)| t).collect();
        let srcs = if sig.params_in_memory {
            let mut fields = Vec::new();
            for t in &tys {
                fields.push(gen_.layout(t)?);
            }
            let (size, align) = record_layout(&fields);
            let _ = writeln!(prologue, "        let at = u64::from(p0 as u32);");
            let _ = writeln!(
                prologue,
                "        glue_abi::check(m.len(), at, {size}, {align});"
            );
            gen_.field_srcs(&tys, &Src::Mem("at".to_string()))?
        } else {
            if !sig.params.is_empty() {
                let slots: Vec<String> = sig
                    .params
                    .iter()
                    .enumerate()
                    .map(|(i, flat)| to_slot(*flat, &format!("p{i}")))
                    .collect();
                let _ = writeln!(out, "        let s = [{}];", slots.join(", "));
            }
            gen_.field_srcs(&tys, &Src::Flat(0))?
        };
        if reads_memory {
            let _ = writeln!(out, "        let m: &[u8] = g.memory();");
            let _ = writeln!(out, "        let lb = glue_abi::Budget::new(m.len());");
        }
        out.push_str(&prologue);
        let mut args = Vec::new();
        for (i, (ty, src)) in tys.iter().zip(&srcs).enumerate() {
            let _ = writeln!(out, "        let a{i} = {};", gen_.lift(ty, src)?);
            args.push(format!("a{i}"));
        }

        let call = format!("self.0.{host}({})", args.join(", "));
        match (&f.result, sig.results.first()) {
            (Some(ty), _) if sig.result_in_memory => {
                let (size, align) = gen_.layout(ty)?;
                let retptr = format!("p{}", sig.params.len().saturating_sub(1));
                let _ = writeln!(out, "        let r = {call};");
                let _ = writeln!(out, "        let at = u64::from({retptr} as u32);");
                let _ = writeln!(
                    out,
                    "        glue_abi::check(g.memory().len(), at, {size}, {align});"
                );
                let _ = writeln!(out, "        {}", gen_.store(ty, "r", "at")?);
            }
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
        // Results of an import allocate through the import's `realloc`.
        let allocated = std::mem::replace(&mut gen_.allocates, false);
        match slot.and_then(|at| reallocs.get_mut(at)) {
            Some(entry) => entry.1 |= allocated,
            None => default_allocates |= allocated,
        }
    }
    out.push_str("}\n");
    // The lifts' own results go through `cabi_realloc`.
    gen_.alloc = ALLOC_MODULE.to_string();
    if let Some((exports, export_names)) = exports {
        write_exports(out, &mut gen_, exports, export_names)?;
    }
    if gen_.writes_memory {
        out.push_str(MEM_HELPERS);
    }
    for (n, (realloc, allocates)) in reallocs.iter().enumerate() {
        if *allocates {
            out.push_str(&import_alloc_helpers(realloc, &import_alloc_module(n)));
        }
    }
    if default_allocates || gen_.allocates {
        out.push_str(ALLOC_HELPERS);
    }
    Ok(())
}

/// The core module's `realloc` export the glue's allocation helper calls.
const REALLOC: &str = "cabi_realloc";

/// The `named_exports` method of `Instance` calling the core export `core_export`.
fn export_method(core_export: &str) -> Option<String> {
    assign_export_names([(core_export, 0)]).into_iter().next()
}

/// Appends `pub trait Exports` (one method per export, named `export_names[i]`) and
/// `impl<H: Host> Exports for Instance<Glue<H>>`: the import direction mirrored. Each method
/// lowers its parameters (`lower`, or `store` into a record when they do not fit the flat
/// limit), calls the core export, lifts the result (`lift`) and then calls the lift's
/// `post-return`.
///
/// # Errors
///
/// A type that expands past the node budget, or a function that allocates in the guest
/// through a `realloc` other than `cabi_realloc` (the allocation helper's).
fn write_exports(
    out: &mut String,
    gen_: &mut Gen<'_>,
    exports: &[WitExport],
    export_names: &[String],
) -> Result<(), TranspileError> {
    out.push_str(
        "\n/// Typed calls of the functions the component exports: the host lowers the arguments\n\
         /// and lifts the result (canonical ABI). A guest trap is a panic.\n\
         pub trait Exports {\n",
    );
    for (e, name) in exports.iter().zip(export_names) {
        let f = &e.func;
        let idents = unique_idents(f.params.iter().map(|(n, _)| n.as_str()));
        let _ = writeln!(out, "    /// {}", doc_text(&f.interface, &f.name));
        let sig = typed_signature(gen_.records, name, f, &idents)?;
        let _ = writeln!(out, "{sig};");
    }
    out.push_str(
        "}\n\n#[allow(unused_variables, unused_parens, unused_mut)]\n\
         impl<H: Host> Exports for Instance<Glue<H>> {\n",
    );
    let mut forwarders = String::new();
    for (e, name) in exports.iter().zip(export_names) {
        write_export(out, &mut forwarders, gen_, e, name)?;
    }
    out.push_str("}\n");
    // `Exports` is not in scope here, so a core export the transpiled `Instance` lacks under
    // this name is a compile error, not a call of the trait method itself.
    let _ = write!(
        out,
        "\nmod glue_core {{\n    use super::{{Glue, Host, Instance}};\n{forwarders}}}\n"
    );
    Ok(())
}

/// One method of the `Exports` impl, and its forwarder to the core export of the same name
/// (the `named_exports` method of `Instance`) in `forwarders`.
fn write_export(
    out: &mut String,
    forwarders: &mut String,
    gen_: &mut Gen<'_>,
    e: &WitExport,
    name: &str,
) -> Result<(), TranspileError> {
    let f = &e.func;
    let mut sig_budget = Budget::new();
    let sig = core_signature(f, &mut sig_budget)?;
    // Lifting differs from lowering for the result: it is the pointer the core function
    // returns, not an extra parameter.
    let mut core_params = sig.params.clone();
    let result_flat = if sig.result_in_memory {
        core_params.pop();
        Some(Flat::I32)
    } else {
        sig.results.first().copied()
    };
    let tys: Vec<&WitType> = f.params.iter().map(|(_, t)| t).collect();
    let idents = unique_idents(f.params.iter().map(|(n, _)| n.as_str()));

    let head = typed_signature(gen_.records, name, f, &idents)?;
    let _ = writeln!(out, "    /// {}", doc_text(&f.interface, &f.name));
    let _ = writeln!(out, "{head} {{");
    let _ = writeln!(out, "        let g = self;");

    let allocated_before = std::mem::replace(&mut gen_.allocates, false);
    let args = if sig.params_in_memory {
        let mut fields = Vec::new();
        for t in &tys {
            fields.push(gen_.layout(t)?);
        }
        let (size, align) = record_layout(&fields);
        gen_.allocates = true;
        gen_.writes_memory = true;
        let _ = writeln!(
            out,
            "        let at = glue_alloc::alloc(g, {align}, 1, {size});"
        );
        let offsets = field_offsets(&tys, &mut gen_.budget)?;
        for ((ty, ident), off) in tys.iter().zip(&idents).zip(offsets) {
            let _ = writeln!(
                out,
                "        {}",
                gen_.store(ty, ident, &offset_expr("at", off))?
            );
        }
        vec!["at as u32 as i32".to_string()]
    } else {
        if !core_params.is_empty() {
            let _ = writeln!(out, "        let mut out = [0u64; {}];", core_params.len());
        }
        let mut base = 0usize;
        for (ty, ident) in tys.iter().zip(&idents) {
            let _ = writeln!(out, "        {}", gen_.lower(ty, ident, base)?);
            base = base.saturating_add(gen_.flat_len(ty)?);
        }
        core_params
            .iter()
            .enumerate()
            .map(|(i, flat)| from_slot(*flat, &format!("out[{i}]")))
            .collect()
    };
    let allocates = std::mem::replace(&mut gen_.allocates, allocated_before);
    gen_.allocates |= allocates;
    let realloc = e.realloc.as_deref().and_then(export_method);
    if allocates && realloc.as_deref() != Some(REALLOC) {
        return Err(unsupported(format!(
            "export `{}` (core module {}): allocating guest memory needs the lift's realloc to \
             be `{REALLOC}` (the lift names {}). Lifts that allocate through different `realloc` \
             functions (several core modules each with their own) are not supported yet",
            e.core_export,
            e.core_module,
            realloc.map_or_else(|| "none".to_string(), |i| format!("`{i}`")),
        )));
    }

    let post = e.post_return.as_deref().and_then(export_method);
    let mut forwarder = format!("    pub fn {name}<H: Host>(g: &mut Instance<Glue<H>>");
    let mut forwarded = Vec::new();
    for (i, flat) in core_params.iter().enumerate() {
        let _ = write!(forwarder, ", p{i}: {}", core_rust(*flat));
        forwarded.push(format!("p{i}"));
    }
    forwarder.push(')');
    if let Some(flat) = result_flat {
        let _ = write!(forwarder, " -> {}", core_rust(flat));
    }
    let _ = writeln!(forwarder, " {{ g.{name}({}) }}", forwarded.join(", "));
    forwarders.push_str(&forwarder);
    let call = format!("glue_core::{name}(g, {})", args.join(", "));
    let post_call = |arg: &str| {
        post.as_ref()
            .map(|p| format!("        g.{p}({arg});\n"))
            .unwrap_or_default()
    };
    match (&f.result, result_flat) {
        (None, _) => {
            let _ = writeln!(out, "        {call};");
            out.push_str(&post_call(""));
        }
        (Some(ty), Some(flat)) => {
            let _ = writeln!(out, "        let r = {call};");
            if sig.result_in_memory {
                let (size, align) = gen_.layout(ty)?;
                let _ = writeln!(out, "        let at = u64::from(r as u32);");
                let _ = writeln!(out, "        let m: &[u8] = g.memory();");
                let _ = writeln!(
                    out,
                    "        glue_abi::check(m.len(), at, {size}, {align});"
                );
                let _ = writeln!(out, "        let lb = glue_abi::Budget::new(m.len());");
                let lifted = gen_.lift(ty, &Src::Mem("at".to_string()))?;
                let _ = writeln!(out, "        let v = {lifted};");
            } else {
                let _ = writeln!(out, "        let s = [{}];", to_slot(flat, "r"));
                let lifted = gen_.lift(ty, &Src::Flat(0))?;
                let _ = writeln!(out, "        let v = {lifted};");
            }
            out.push_str(&post_call("r"));
            out.push_str("        v\n");
        }
        // A result with no core value (an empty record or flags): nothing to read.
        (Some(ty), None) => {
            let _ = writeln!(out, "        {call};");
            let lifted = gen_.lift(ty, &Src::Flat(0))?;
            let _ = writeln!(out, "        let v = {lifted};");
            out.push_str(&post_call(""));
            out.push_str("        v\n");
        }
    }
    out.push_str("    }\n");
    Ok(())
}
