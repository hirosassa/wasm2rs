use std::borrow::Cow;
use std::collections::HashSet;

use super::Rt;

pub(super) fn rt_name(rt: Rt) -> &'static str {
    match rt {
        Rt::F32Min => "f32_min",
        Rt::F32Max => "f32_max",
        Rt::F64Min => "f64_min",
        Rt::F64Max => "f64_max",
        Rt::QuietF32 => "quiet_f32",
        Rt::QuietF64 => "quiet_f64",
        Rt::I32TruncF32S => "i32_trunc_f32_s",
        Rt::I32TruncF32U => "i32_trunc_f32_u",
        Rt::I32TruncF64S => "i32_trunc_f64_s",
        Rt::I32TruncF64U => "i32_trunc_f64_u",
        Rt::I64TruncF32S => "i64_trunc_f32_s",
        Rt::I64TruncF32U => "i64_trunc_f32_u",
        Rt::I64TruncF64S => "i64_trunc_f64_s",
        Rt::I64TruncF64U => "i64_trunc_f64_u",
        Rt::SplatI8x16 => "i8x16_splat",
        Rt::SplatI16x8 => "i16x8_splat",
        Rt::SplatI32x4 => "i32x4_splat",
        Rt::SplatI64x2 => "i64x2_splat",
        Rt::SplatF32x4 => "f32x4_splat",
        Rt::SplatF64x2 => "f64x2_splat",
        Rt::TrapUnreachable => "trap_unreachable",
        Rt::TrapIndirectTypeMismatch => "trap_indirect_type_mismatch",
        Rt::DelegateExc => "Wasm2RsDelegate",
        Rt::Tc => "Tc",
        Rt::CallDepth => "CallDepth",
        Rt::TrapCallStackExhausted => "trap_call_stack_exhausted",
        Rt::TrapType => "Wasm2RsTrap",
        Rt::TrapInvalidConversion => "trap_invalid_conversion",
        Rt::TrapIntegerOverflow => "trap_integer_overflow",
    }
}

/// Whether `name` is a runtime helper, for reserving it against export names.
pub(super) fn is_rt_name(name: &str) -> bool {
    RT_ORDER.iter().any(|rt| rt_name(*rt) == name)
}

/// All runtime helpers (free functions and support types), in a deterministic
/// emission order.
const RT_ORDER: [Rt; 29] = [
    Rt::TrapType,
    Rt::DelegateExc,
    Rt::CallDepth,
    Rt::TrapCallStackExhausted,
    Rt::Tc,
    Rt::TrapUnreachable,
    Rt::TrapIndirectTypeMismatch,
    Rt::TrapInvalidConversion,
    Rt::TrapIntegerOverflow,
    Rt::F32Min,
    Rt::F32Max,
    Rt::F64Min,
    Rt::F64Max,
    Rt::QuietF32,
    Rt::QuietF64,
    Rt::I32TruncF32S,
    Rt::I32TruncF32U,
    Rt::I32TruncF64S,
    Rt::I32TruncF64U,
    Rt::I64TruncF32S,
    Rt::I64TruncF32U,
    Rt::I64TruncF64S,
    Rt::I64TruncF64U,
    Rt::SplatI8x16,
    Rt::SplatI16x8,
    Rt::SplatI32x4,
    Rt::SplatI64x2,
    Rt::SplatF32x4,
    Rt::SplatF64x2,
];

/// Render the used runtime helpers as module-scope items, in
/// [`RT_ORDER`], separated by blank lines. Returns an empty string if none.
///
/// With `typed` the helper traps raise a `Wasm2RsTrap` payload, so the type (and
/// the cold trap functions the truncations call) join the helpers that need them.
pub(super) fn render_rt_helpers(used: &HashSet<Rt>, typed: bool) -> String {
    let mut used = Cow::Borrowed(used);
    let mut variants: Vec<TrapVariant> = Vec::new();
    if typed {
        for (rt, variant) in [
            (Rt::TrapUnreachable, TrapVariant::Unreachable),
            (
                Rt::TrapIndirectTypeMismatch,
                TrapVariant::IndirectCallTypeMismatch,
            ),
            (Rt::TrapCallStackExhausted, TrapVariant::CallStackExhausted),
        ] {
            if used.contains(&rt) {
                variants.push(variant);
            }
        }
        let uses_trunc = used.iter().any(|rt| TRUNC_RTS.contains(rt));
        if uses_trunc {
            variants.push(TrapVariant::InvalidConversionToInteger);
            variants.push(TrapVariant::IntegerOverflow);
        }
        if !variants.is_empty() {
            let owned = used.to_mut();
            owned.insert(Rt::TrapType);
            if uses_trunc {
                owned.insert(Rt::TrapInvalidConversion);
                owned.insert(Rt::TrapIntegerOverflow);
            }
        }
    }
    let mut blocks: Vec<String> = Vec::new();
    for rt in RT_ORDER {
        if used.contains(&rt) {
            blocks.push(rt_lines(rt, typed, &variants).join("\n"));
        }
    }
    let mut out = blocks.join("\n\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

/// The trapping float->int truncation helpers.
const TRUNC_RTS: [Rt; 8] = [
    Rt::I32TruncF32S,
    Rt::I32TruncF32U,
    Rt::I32TruncF64S,
    Rt::I32TruncF64U,
    Rt::I64TruncF32S,
    Rt::I64TruncF32U,
    Rt::I64TruncF64S,
    Rt::I64TruncF64U,
];

/// A variant of the typed trap payload `Wasm2RsTrap`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TrapVariant {
    Unreachable,
    IndirectCallTypeMismatch,
    CallStackExhausted,
    InvalidConversionToInteger,
    IntegerOverflow,
}

impl TrapVariant {
    fn name(self) -> &'static str {
        match self {
            TrapVariant::Unreachable => "Unreachable",
            TrapVariant::IndirectCallTypeMismatch => "IndirectCallTypeMismatch",
            TrapVariant::CallStackExhausted => "CallStackExhausted",
            TrapVariant::InvalidConversionToInteger => "InvalidConversionToInteger",
            TrapVariant::IntegerOverflow => "IntegerOverflow",
        }
    }

    /// The message the string `panic!` of the untyped output carries.
    fn message(self) -> &'static str {
        match self {
            TrapVariant::Unreachable => "unreachable",
            TrapVariant::IndirectCallTypeMismatch => "indirect call type mismatch",
            TrapVariant::CallStackExhausted => "call stack exhausted",
            TrapVariant::InvalidConversionToInteger => "invalid conversion to integer",
            TrapVariant::IntegerOverflow => "integer overflow",
        }
    }
}

/// The `Wasm2RsTrap` definition with just the `variants` the helpers raise.
fn trap_type_lines(variants: &[TrapVariant]) -> Vec<String> {
    let mut lines = vec![
        "/// The payload of a trap raised by a runtime helper (`typed_traps`). It is raised"
            .to_string(),
        "/// with `resume_unwind`, so downcast the `catch_unwind` payload to recognise it."
            .to_string(),
        "#[allow(dead_code)]".to_string(),
        "#[derive(Debug, Clone, Copy, PartialEq, Eq)]".to_string(),
        "pub enum Wasm2RsTrap {".to_string(),
    ];
    lines.extend(variants.iter().map(|v| format!("    {},", v.name())));
    lines.extend(
        [
            "}",
            "impl ::std::fmt::Display for Wasm2RsTrap {",
            "    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {",
            "        f.write_str(match self {",
        ]
        .map(String::from),
    );
    lines.extend(variants.iter().map(|v| {
        format!(
            "            Wasm2RsTrap::{} => \"{}\",",
            v.name(),
            v.message()
        )
    }));
    lines.extend(
        [
            "        })",
            "    }",
            "}",
            "impl ::std::error::Error for Wasm2RsTrap {}",
        ]
        .map(String::from),
    );
    lines
}

/// An out-of-line trap helper: a string `panic!` or, when typed, a `resume_unwind`
/// of the `variant` payload (which skips the panic hook).
fn trap_fn_lines(name: &str, variant: TrapVariant, typed: bool) -> Vec<String> {
    let raise = if typed {
        format!(
            "    ::std::panic::resume_unwind(Box::new(Wasm2RsTrap::{}))",
            variant.name()
        )
    } else {
        format!("    panic!(\"{}\")", variant.message())
    };
    vec![
        "#[cold]".to_string(),
        "#[inline(never)]".to_string(),
        format!("fn {name}() -> ! {{"),
        raise,
        "}".to_string(),
    ]
}

/// The source lines of one runtime helper, dispatched to the min/max or the
/// trapping-truncation template.
fn rt_lines(rt: Rt, typed: bool, variants: &[TrapVariant]) -> Vec<String> {
    match rt {
        Rt::TrapType => trap_type_lines(variants),
        Rt::TrapInvalidConversion => trap_fn_lines(
            "trap_invalid_conversion",
            TrapVariant::InvalidConversionToInteger,
            typed,
        ),
        Rt::TrapIntegerOverflow => {
            trap_fn_lines("trap_integer_overflow", TrapVariant::IntegerOverflow, typed)
        }
        Rt::DelegateExc => source_lines(include_str!("rt_src/delegate.rs")),
        Rt::Tc => source_lines(include_str!("rt_src/tc.rs")),
        // `CALL_DEPTH` is upper-case, which a sanitized export name never is.
        Rt::CallDepth => source_lines(include_str!("rt_src/call_depth.rs")),
        Rt::TrapCallStackExhausted => trap_fn_lines(
            "trap_call_stack_exhausted",
            TrapVariant::CallStackExhausted,
            typed,
        ),
        Rt::TrapUnreachable => trap_fn_lines("trap_unreachable", TrapVariant::Unreachable, typed),
        Rt::TrapIndirectTypeMismatch => trap_fn_lines(
            "trap_indirect_type_mismatch",
            TrapVariant::IndirectCallTypeMismatch,
            typed,
        ),
        // wasm `min`/`max` return NaN if either operand is NaN, and when the operands
        // are equal (notably ±0) `min` yields the negatively-signed and `max` the
        // positively-signed value, differing from Rust's `f32::min`/`max`.
        Rt::F32Min => source_lines(include_str!("rt_src/float_min.rs")),
        Rt::F32Max => source_lines(include_str!("rt_src/float_max.rs")),
        Rt::F64Min => widen(include_str!("rt_src/float_min.rs")),
        Rt::F64Max => widen(include_str!("rt_src/float_max.rs")),
        // A NaN result of `floor`/`ceil`/`trunc`/`nearest` (and of `demote`/`promote`,
        // which LLVM can fold to the identity) must be quiet, but a host without
        // hardware rounding instructions (baseline x86_64) returns a signaling NaN
        // input unchanged. `r + r` quiets a NaN (keeping sign and payload) and only
        // runs for NaN, so ordinary results are untouched.
        Rt::QuietF32 => source_lines(include_str!("rt_src/float_quiet.rs")),
        Rt::QuietF64 => widen(include_str!("rt_src/float_quiet.rs")),
        Rt::I32TruncF32S => trunc_lines(
            typed,
            "i32_trunc_f32_s",
            "f32",
            "i32",
            TRUNC_I32_S_F32,
            "x as i32",
        ),
        Rt::I32TruncF32U => trunc_lines(
            typed,
            "i32_trunc_f32_u",
            "f32",
            "i32",
            TRUNC_U_F32_32,
            "x as u32 as i32",
        ),
        Rt::I32TruncF64S => trunc_lines(
            typed,
            "i32_trunc_f64_s",
            "f64",
            "i32",
            TRUNC_I32_S_F64,
            "x as i32",
        ),
        Rt::I32TruncF64U => trunc_lines(
            typed,
            "i32_trunc_f64_u",
            "f64",
            "i32",
            TRUNC_U_F64_32,
            "x as u32 as i32",
        ),
        Rt::I64TruncF32S => trunc_lines(
            typed,
            "i64_trunc_f32_s",
            "f32",
            "i64",
            TRUNC_I64_S_F32,
            "x as i64",
        ),
        Rt::I64TruncF32U => trunc_lines(
            typed,
            "i64_trunc_f32_u",
            "f32",
            "i64",
            TRUNC_U_F32_64,
            "x as u64 as i64",
        ),
        Rt::I64TruncF64S => trunc_lines(
            typed,
            "i64_trunc_f64_s",
            "f64",
            "i64",
            TRUNC_I64_S_F64,
            "x as i64",
        ),
        Rt::I64TruncF64U => trunc_lines(
            typed,
            "i64_trunc_f64_u",
            "f64",
            "i64",
            TRUNC_U_F64_64,
            "x as u64 as i64",
        ),
        Rt::SplatI8x16 => splat_lines("i8x16_splat", "i32", "x as u8 as u128", 8),
        Rt::SplatI16x8 => splat_lines("i16x8_splat", "i32", "x as u16 as u128", 16),
        Rt::SplatI32x4 => splat_lines("i32x4_splat", "i32", "x as u32 as u128", 32),
        Rt::SplatI64x2 => splat_lines("i64x2_splat", "i64", "x as u64 as u128", 64),
        Rt::SplatF32x4 => splat_lines("f32x4_splat", "f32", "x.to_bits() as u128", 32),
        Rt::SplatF64x2 => splat_lines("f64x2_splat", "f64", "x.to_bits() as u128", 64),
    }
}

/// A lane-splat helper: place the scalar in the lowest `lane_bits`-wide lane,
/// then double the filled width until all 128 bits are covered. Each `l |= l <<
/// n` copies the already-filled low `n` bits into the next `n` bits, so after
/// the last shift every lane holds the scalar.
fn splat_lines(name: &str, scalar_ty: &str, low_lane: &str, lane_bits: u32) -> Vec<String> {
    let mut lines = vec![
        format!("fn {name}(x: {scalar_ty}) -> u128 {{"),
        format!("    let mut l = {low_lane};"),
    ];
    let mut width = lane_bits;
    while width < 128 {
        lines.push(format!("    l |= l << {width};"));
        width *= 2;
    }
    lines.push("    l".to_string());
    lines.push("}".to_string());
    lines
}

// The in-range predicates for the trapping truncations, following wasm2c's
// proven bounds. Signed f32 sources use `>=` on the exact `-2^N` lower bound
// (the next representable f32 below already truncates out of range); signed
// i32-from-f64 needs a strict `> -2^31 - 1` because f64 can represent values
// between `-2^31 - 1` and `-2^31`. Unsigned sources reject anything `<= -1`.
const TRUNC_I32_S_F32: &str = "x >= -2147483648.0f32 && x < 2147483648.0f32";
const TRUNC_I32_S_F64: &str = "x > -2147483649.0f64 && x < 2147483648.0f64";
const TRUNC_I64_S_F32: &str = "x >= -9223372036854775808.0f32 && x < 9223372036854775808.0f32";
const TRUNC_I64_S_F64: &str = "x >= -9223372036854775808.0f64 && x < 9223372036854775808.0f64";
const TRUNC_U_F32_32: &str = "x > -1.0f32 && x < 4294967296.0f32";
const TRUNC_U_F64_32: &str = "x > -1.0f64 && x < 4294967296.0f64";
const TRUNC_U_F32_64: &str = "x > -1.0f32 && x < 18446744073709551616.0f32";
const TRUNC_U_F64_64: &str = "x > -1.0f64 && x < 18446744073709551616.0f64";

/// A non-saturating float->int truncation helper: trap on NaN, trap when the
/// value is outside `range`, else convert via `cast`.
fn trunc_lines(
    typed: bool,
    name: &str,
    ft: &str,
    it: &str,
    range: &str,
    cast: &str,
) -> Vec<String> {
    // The typed output calls the shared cold trap functions; the untyped output
    // keeps its inline `panic!`s.
    let (nan_trap, range_trap) = if typed {
        ("trap_invalid_conversion();", "trap_integer_overflow();")
    } else {
        (
            "panic!(\"invalid conversion to integer\");",
            "panic!(\"integer overflow\");",
        )
    };
    vec![
        format!("fn {name}(x: {ft}) -> {it} {{"),
        "    if x.is_nan() {".to_string(),
        format!("        {nan_trap}"),
        "    }".to_string(),
        format!("    if !({range}) {{"),
        format!("        {range_trap}"),
        "    }".to_string(),
        format!("    {cast}"),
        "}".to_string(),
    ]
}

/// The lines of a verbatim `rt_src` file.
pub(super) fn source_lines(src: &str) -> Vec<String> {
    src.lines().map(str::to_string).collect()
}

/// The `f64` instance of an `rt_src` helper written for `f32`.
fn widen(src: &str) -> Vec<String> {
    source_lines(&src.replace("f32", "f64"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(rt: Rt) -> String {
        render_rt_helpers(&HashSet::from([rt]), false)
    }

    #[test]
    fn verbatim_files_are_emitted_as_they_are() {
        assert_eq!(render(Rt::DelegateExc), include_str!("rt_src/delegate.rs"));
        assert_eq!(render(Rt::Tc), include_str!("rt_src/tc.rs"));
    }

    #[test]
    fn float_helpers_are_named_by_rt_name_and_f64_is_the_widened_f32_instance() {
        for rt in [
            Rt::F32Min,
            Rt::F32Max,
            Rt::F64Min,
            Rt::F64Max,
            Rt::QuietF32,
            Rt::QuietF64,
        ] {
            let out = render(rt);
            assert!(out.starts_with(&format!("fn {}(", rt_name(rt))), "{out}");
            if matches!(rt, Rt::F64Min | Rt::F64Max | Rt::QuietF64) {
                assert!(!out.contains("f32"), "{out}");
            }
        }
    }
}
