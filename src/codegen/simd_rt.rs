//! Lane-wise SIMD runtime helpers: module-scope free functions over `u128`
//! (a v128), each splitting the register into lanes, applying a per-lane scalar
//! operation, and repacking. Unlike the scalar [`super::Rt`] helpers there are
//! many and they are highly regular, so they are described by a table ([`LANE`])
//! and generated from a single template rather than written out by hand. A
//! function tracks the ones it uses by name (`used_simd`), matching how it
//! tracks [`super::Helper`]/[`super::Rt`] dependencies.

use std::collections::HashSet;

/// A lane helper's operand shape, which fixes its signature and how each lane's
/// inputs are read.
#[derive(Clone, Copy)]
enum Shape {
    /// `fn(a: u128)` — one vector, read as `x` per lane.
    Unary,
    /// `fn(a: u128, b: u128)` — two vectors, read as `x` and `y` per lane.
    Binary,
    /// `fn(a: u128, s: i32)` — a vector and a scalar shift count, reduced to
    /// `k = s mod lane-width` once before the loop (wasm masks the count).
    Shift,
}

/// How a lane helper combines the bound lane value(s) into the per-lane result,
/// written in terms of `x` (and `y` for binary lanes, `k` for shifts).
enum Combine {
    /// Binary infix: `x <op> y` (e.g. `+`, `/`).
    Infix(&'static str),
    /// A method call: `x.<m>(y)` for binary lanes, `x.<m>()` for unary.
    Method(&'static str),
    /// wasm `min`/`max`: NaN-propagating, and for equal operands (notably ±0)
    /// picks by sign. `order` is `<` for min / `>` for max; `equal` chooses the
    /// winner when `x == y`. Mirrors the scalar `f32_min`/`f32_max` helpers.
    MinMax {
        equal: &'static str,
        order: &'static str,
    },
    /// A custom single expression in `x` and `y` (used for `pmin`/`pmax`).
    Expr(&'static str),
    /// wasm `avgr_u`: unsigned rounding average `(x + y + 1) / 2`. The sum is
    /// computed in `u32` (wide enough for u8/u16 lanes) to avoid overflow, then
    /// cast back to the lane type.
    Avgr,
    /// A lane comparison `x <op> y` yielding an all-ones lane mask when true and
    /// zero when false, as wasm's vector predicates require.
    Compare(&'static str),
    /// A shift `x <op> k` by the (pre-masked) scalar count.
    Shift(&'static str),
}

/// For equal (±0) operands, `min` keeps the negatively-signed lane, `max` the
/// positively-signed one.
const MIN_EQUAL: &str = "if x.is_sign_negative() { x } else { y }";
const MAX_EQUAL: &str = "if x.is_sign_negative() { y } else { x }";

/// One lane helper: `name` is the emitted function name, `elem` the Rust type a
/// lane is read as, `bytes` its width, `shape` its operand signature, and
/// `combine` how the lane result is formed. `elem` is signed for arithmetic
/// (wrapping add/sub/mul/neg are bit-identical for signed and unsigned) but
/// picks signedness to match the operation for comparisons/shifts, and is a
/// float type for float lanes.
struct Lane {
    name: &'static str,
    elem: &'static str,
    bytes: u32,
    shape: Shape,
    combine: Combine,
}

const fn lane(
    name: &'static str,
    elem: &'static str,
    bytes: u32,
    shape: Shape,
    combine: Combine,
) -> Lane {
    Lane {
        name,
        elem,
        bytes,
        shape,
        combine,
    }
}

/// All lane helpers, in a deterministic emission order. `#[rustfmt::skip]` keeps
/// each entry on one line so the table reads as a table (rustfmt would otherwise
/// explode the longer entries across several lines each).
#[rustfmt::skip]
const LANE: &[Lane] = &[
    // Integer wrapping arithmetic.
    lane("i8x16_add", "i8", 1, Shape::Binary, Combine::Method("wrapping_add")),
    lane("i16x8_add", "i16", 2, Shape::Binary, Combine::Method("wrapping_add")),
    lane("i32x4_add", "i32", 4, Shape::Binary, Combine::Method("wrapping_add")),
    lane("i64x2_add", "i64", 8, Shape::Binary, Combine::Method("wrapping_add")),
    lane("i8x16_sub", "i8", 1, Shape::Binary, Combine::Method("wrapping_sub")),
    lane("i16x8_sub", "i16", 2, Shape::Binary, Combine::Method("wrapping_sub")),
    lane("i32x4_sub", "i32", 4, Shape::Binary, Combine::Method("wrapping_sub")),
    lane("i64x2_sub", "i64", 8, Shape::Binary, Combine::Method("wrapping_sub")),
    lane("i16x8_mul", "i16", 2, Shape::Binary, Combine::Method("wrapping_mul")),
    lane("i32x4_mul", "i32", 4, Shape::Binary, Combine::Method("wrapping_mul")),
    lane("i64x2_mul", "i64", 8, Shape::Binary, Combine::Method("wrapping_mul")),
    lane("i8x16_neg", "i8", 1, Shape::Unary, Combine::Method("wrapping_neg")),
    lane("i16x8_neg", "i16", 2, Shape::Unary, Combine::Method("wrapping_neg")),
    lane("i32x4_neg", "i32", 4, Shape::Unary, Combine::Method("wrapping_neg")),
    lane("i64x2_neg", "i64", 8, Shape::Unary, Combine::Method("wrapping_neg")),
    // Integer saturating add/sub. Unlike wrapping these depend on signedness, so
    // `_s` reads signed lanes and `_u` unsigned; the method clamps to that range.
    lane("i8x16_add_sat_s", "i8", 1, Shape::Binary, Combine::Method("saturating_add")),
    lane("i8x16_add_sat_u", "u8", 1, Shape::Binary, Combine::Method("saturating_add")),
    lane("i16x8_add_sat_s", "i16", 2, Shape::Binary, Combine::Method("saturating_add")),
    lane("i16x8_add_sat_u", "u16", 2, Shape::Binary, Combine::Method("saturating_add")),
    lane("i8x16_sub_sat_s", "i8", 1, Shape::Binary, Combine::Method("saturating_sub")),
    lane("i8x16_sub_sat_u", "u8", 1, Shape::Binary, Combine::Method("saturating_sub")),
    lane("i16x8_sub_sat_s", "i16", 2, Shape::Binary, Combine::Method("saturating_sub")),
    lane("i16x8_sub_sat_u", "u16", 2, Shape::Binary, Combine::Method("saturating_sub")),
    // Q15 fixed-point rounding multiply: (a*b + 0x4000) >> 15, saturated to i16.
    // The product needs an i32 intermediate, so the lane read stays i16.
    lane("i16x8_q15mulr_sat_s", "i16", 2, Shape::Binary, Combine::Expr(
        "(((x as i32) * (y as i32) + 0x4000) >> 15).clamp(i16::MIN as i32, i16::MAX as i32) as i16",
    )),
    // Integer lane abs (wrapping, so iN::MIN maps to itself), unsigned rounding
    // average, and per-byte population count.
    lane("i8x16_abs", "i8", 1, Shape::Unary, Combine::Method("wrapping_abs")),
    lane("i16x8_abs", "i16", 2, Shape::Unary, Combine::Method("wrapping_abs")),
    lane("i32x4_abs", "i32", 4, Shape::Unary, Combine::Method("wrapping_abs")),
    lane("i64x2_abs", "i64", 8, Shape::Unary, Combine::Method("wrapping_abs")),
    lane("i8x16_avgr_u", "u8", 1, Shape::Binary, Combine::Avgr),
    lane("i16x8_avgr_u", "u16", 2, Shape::Binary, Combine::Avgr),
    lane("i8x16_popcnt", "u8", 1, Shape::Unary, Combine::Expr("x.count_ones() as u8")),
    // Float arithmetic.
    lane("f32x4_add", "f32", 4, Shape::Binary, Combine::Infix("+")),
    lane("f64x2_add", "f64", 8, Shape::Binary, Combine::Infix("+")),
    lane("f32x4_sub", "f32", 4, Shape::Binary, Combine::Infix("-")),
    lane("f64x2_sub", "f64", 8, Shape::Binary, Combine::Infix("-")),
    lane("f32x4_mul", "f32", 4, Shape::Binary, Combine::Infix("*")),
    lane("f64x2_mul", "f64", 8, Shape::Binary, Combine::Infix("*")),
    lane("f32x4_div", "f32", 4, Shape::Binary, Combine::Infix("/")),
    lane("f64x2_div", "f64", 8, Shape::Binary, Combine::Infix("/")),
    lane("f32x4_min", "f32", 4, Shape::Binary, Combine::MinMax { equal: MIN_EQUAL, order: "<" }),
    lane("f64x2_min", "f64", 8, Shape::Binary, Combine::MinMax { equal: MIN_EQUAL, order: "<" }),
    lane("f32x4_max", "f32", 4, Shape::Binary, Combine::MinMax { equal: MAX_EQUAL, order: ">" }),
    lane("f64x2_max", "f64", 8, Shape::Binary, Combine::MinMax { equal: MAX_EQUAL, order: ">" }),
    lane("f32x4_pmin", "f32", 4, Shape::Binary, Combine::Expr("if y < x { y } else { x }")),
    lane("f64x2_pmin", "f64", 8, Shape::Binary, Combine::Expr("if y < x { y } else { x }")),
    lane("f32x4_pmax", "f32", 4, Shape::Binary, Combine::Expr("if x < y { y } else { x }")),
    lane("f64x2_pmax", "f64", 8, Shape::Binary, Combine::Expr("if x < y { y } else { x }")),
    // Float unary (per-lane method).
    lane("f32x4_sqrt", "f32", 4, Shape::Unary, Combine::Method("sqrt")),
    lane("f64x2_sqrt", "f64", 8, Shape::Unary, Combine::Method("sqrt")),
    lane("f32x4_ceil", "f32", 4, Shape::Unary, Combine::Method("ceil")),
    lane("f64x2_ceil", "f64", 8, Shape::Unary, Combine::Method("ceil")),
    lane("f32x4_floor", "f32", 4, Shape::Unary, Combine::Method("floor")),
    lane("f64x2_floor", "f64", 8, Shape::Unary, Combine::Method("floor")),
    lane("f32x4_trunc", "f32", 4, Shape::Unary, Combine::Method("trunc")),
    lane("f64x2_trunc", "f64", 8, Shape::Unary, Combine::Method("trunc")),
    lane("f32x4_nearest", "f32", 4, Shape::Unary, Combine::Method("round_ties_even")),
    lane("f64x2_nearest", "f64", 8, Shape::Unary, Combine::Method("round_ties_even")),
    // Integer lane comparisons (all-ones / zero mask). `_u` reads unsigned.
    lane("i8x16_eq", "i8", 1, Shape::Binary, Combine::Compare("==")),
    lane("i8x16_ne", "i8", 1, Shape::Binary, Combine::Compare("!=")),
    lane("i8x16_lt_s", "i8", 1, Shape::Binary, Combine::Compare("<")),
    lane("i8x16_lt_u", "u8", 1, Shape::Binary, Combine::Compare("<")),
    lane("i8x16_gt_s", "i8", 1, Shape::Binary, Combine::Compare(">")),
    lane("i8x16_gt_u", "u8", 1, Shape::Binary, Combine::Compare(">")),
    lane("i8x16_le_s", "i8", 1, Shape::Binary, Combine::Compare("<=")),
    lane("i8x16_le_u", "u8", 1, Shape::Binary, Combine::Compare("<=")),
    lane("i8x16_ge_s", "i8", 1, Shape::Binary, Combine::Compare(">=")),
    lane("i8x16_ge_u", "u8", 1, Shape::Binary, Combine::Compare(">=")),
    lane("i16x8_eq", "i16", 2, Shape::Binary, Combine::Compare("==")),
    lane("i16x8_ne", "i16", 2, Shape::Binary, Combine::Compare("!=")),
    lane("i16x8_lt_s", "i16", 2, Shape::Binary, Combine::Compare("<")),
    lane("i16x8_lt_u", "u16", 2, Shape::Binary, Combine::Compare("<")),
    lane("i16x8_gt_s", "i16", 2, Shape::Binary, Combine::Compare(">")),
    lane("i16x8_gt_u", "u16", 2, Shape::Binary, Combine::Compare(">")),
    lane("i16x8_le_s", "i16", 2, Shape::Binary, Combine::Compare("<=")),
    lane("i16x8_le_u", "u16", 2, Shape::Binary, Combine::Compare("<=")),
    lane("i16x8_ge_s", "i16", 2, Shape::Binary, Combine::Compare(">=")),
    lane("i16x8_ge_u", "u16", 2, Shape::Binary, Combine::Compare(">=")),
    lane("i32x4_eq", "i32", 4, Shape::Binary, Combine::Compare("==")),
    lane("i32x4_ne", "i32", 4, Shape::Binary, Combine::Compare("!=")),
    lane("i32x4_lt_s", "i32", 4, Shape::Binary, Combine::Compare("<")),
    lane("i32x4_lt_u", "u32", 4, Shape::Binary, Combine::Compare("<")),
    lane("i32x4_gt_s", "i32", 4, Shape::Binary, Combine::Compare(">")),
    lane("i32x4_gt_u", "u32", 4, Shape::Binary, Combine::Compare(">")),
    lane("i32x4_le_s", "i32", 4, Shape::Binary, Combine::Compare("<=")),
    lane("i32x4_le_u", "u32", 4, Shape::Binary, Combine::Compare("<=")),
    lane("i32x4_ge_s", "i32", 4, Shape::Binary, Combine::Compare(">=")),
    lane("i32x4_ge_u", "u32", 4, Shape::Binary, Combine::Compare(">=")),
    lane("i64x2_eq", "i64", 8, Shape::Binary, Combine::Compare("==")),
    lane("i64x2_ne", "i64", 8, Shape::Binary, Combine::Compare("!=")),
    lane("i64x2_lt_s", "i64", 8, Shape::Binary, Combine::Compare("<")),
    lane("i64x2_gt_s", "i64", 8, Shape::Binary, Combine::Compare(">")),
    lane("i64x2_le_s", "i64", 8, Shape::Binary, Combine::Compare("<=")),
    lane("i64x2_ge_s", "i64", 8, Shape::Binary, Combine::Compare(">=")),
    // Float lane comparisons (NaN makes only `!=` true).
    lane("f32x4_eq", "f32", 4, Shape::Binary, Combine::Compare("==")),
    lane("f32x4_ne", "f32", 4, Shape::Binary, Combine::Compare("!=")),
    lane("f32x4_lt", "f32", 4, Shape::Binary, Combine::Compare("<")),
    lane("f32x4_gt", "f32", 4, Shape::Binary, Combine::Compare(">")),
    lane("f32x4_le", "f32", 4, Shape::Binary, Combine::Compare("<=")),
    lane("f32x4_ge", "f32", 4, Shape::Binary, Combine::Compare(">=")),
    lane("f64x2_eq", "f64", 8, Shape::Binary, Combine::Compare("==")),
    lane("f64x2_ne", "f64", 8, Shape::Binary, Combine::Compare("!=")),
    lane("f64x2_lt", "f64", 8, Shape::Binary, Combine::Compare("<")),
    lane("f64x2_gt", "f64", 8, Shape::Binary, Combine::Compare(">")),
    lane("f64x2_le", "f64", 8, Shape::Binary, Combine::Compare("<=")),
    lane("f64x2_ge", "f64", 8, Shape::Binary, Combine::Compare(">=")),
    // Lane shifts by a scalar count. `shl`/`shr_u` read unsigned, `shr_s` signed.
    lane("i8x16_shl", "u8", 1, Shape::Shift, Combine::Shift("<<")),
    lane("i16x8_shl", "u16", 2, Shape::Shift, Combine::Shift("<<")),
    lane("i32x4_shl", "u32", 4, Shape::Shift, Combine::Shift("<<")),
    lane("i64x2_shl", "u64", 8, Shape::Shift, Combine::Shift("<<")),
    lane("i8x16_shr_s", "i8", 1, Shape::Shift, Combine::Shift(">>")),
    lane("i16x8_shr_s", "i16", 2, Shape::Shift, Combine::Shift(">>")),
    lane("i32x4_shr_s", "i32", 4, Shape::Shift, Combine::Shift(">>")),
    lane("i64x2_shr_s", "i64", 8, Shape::Shift, Combine::Shift(">>")),
    lane("i8x16_shr_u", "u8", 1, Shape::Shift, Combine::Shift(">>")),
    lane("i16x8_shr_u", "u16", 2, Shape::Shift, Combine::Shift(">>")),
    lane("i32x4_shr_u", "u32", 4, Shape::Shift, Combine::Shift(">>")),
    lane("i64x2_shr_u", "u64", 8, Shape::Shift, Combine::Shift(">>")),
];

/// A width-changing lane helper, where the source and result lane widths differ.
/// `in_elem`/`out_elem` are the source and result lane types (signed for `_s`,
/// unsigned for `_u`) and `in_bytes`/`out_bytes` their widths.
struct Wide {
    name: &'static str,
    in_elem: &'static str,
    out_elem: &'static str,
    in_bytes: u32,
    out_bytes: u32,
    kind: WideKind,
}

/// The two width-changing shapes.
enum WideKind {
    /// `extend_low`/`extend_high`: widen one vector's low or high half to double
    /// width. `off` is the source byte offset of that half (0 low, 8 high).
    Extend { off: u32 },
    /// `narrow`: saturate two vectors' (signed) source lanes to half width and
    /// concatenate, the first vector's lanes then the second's. `min`/`max` are
    /// the saturation bounds as `in_elem` literals.
    Narrow {
        min: &'static str,
        max: &'static str,
    },
    /// `trunc_sat_..._zero`/`demote_..._zero`: convert a single vector's lanes to
    /// a narrower width into the low lanes, zeroing the rest. Input driven like
    /// `Narrow`, but takes one vector and relies on the `as` cast (which saturates
    /// float-to-int: NaN to 0, out of range to the bounds) rather than clamping.
    NarrowZero,
    /// `extmul_low`/`extmul_high`: widen the low or high half (`off`) of both
    /// vectors' lanes and multiply pairwise. The product fits the wider lane.
    ExtMul { off: u32 },
    /// `extadd_pairwise`: widen and sum each adjacent lane pair of one vector.
    ExtAddPairwise,
    /// `dot`: per output lane, sum the two adjacent widened products of both
    /// vectors, wrapping on the (rare) overflow as wasm's two's-complement add.
    Dot,
}

const fn wide(
    name: &'static str,
    in_elem: &'static str,
    out_elem: &'static str,
    in_bytes: u32,
    out_bytes: u32,
    kind: WideKind,
) -> Wide {
    Wide {
        name,
        in_elem,
        out_elem,
        in_bytes,
        out_bytes,
        kind,
    }
}

/// All width-changing lane helpers, in emission order. `#[rustfmt::skip]` keeps
/// one entry per line, matching [`LANE`].
#[rustfmt::skip]
const WIDE: &[Wide] = &[
    wide("i16x8_extend_low_i8x16_s",  "i8",  "i16", 1, 2, WideKind::Extend { off: 0 }),
    wide("i16x8_extend_high_i8x16_s", "i8",  "i16", 1, 2, WideKind::Extend { off: 8 }),
    wide("i16x8_extend_low_i8x16_u",  "u8",  "u16", 1, 2, WideKind::Extend { off: 0 }),
    wide("i16x8_extend_high_i8x16_u", "u8",  "u16", 1, 2, WideKind::Extend { off: 8 }),
    wide("i32x4_extend_low_i16x8_s",  "i16", "i32", 2, 4, WideKind::Extend { off: 0 }),
    wide("i32x4_extend_high_i16x8_s", "i16", "i32", 2, 4, WideKind::Extend { off: 8 }),
    wide("i32x4_extend_low_i16x8_u",  "u16", "u32", 2, 4, WideKind::Extend { off: 0 }),
    wide("i32x4_extend_high_i16x8_u", "u16", "u32", 2, 4, WideKind::Extend { off: 8 }),
    wide("i64x2_extend_low_i32x4_s",  "i32", "i64", 4, 8, WideKind::Extend { off: 0 }),
    wide("i64x2_extend_high_i32x4_s", "i32", "i64", 4, 8, WideKind::Extend { off: 8 }),
    wide("i64x2_extend_low_i32x4_u",  "u32", "u64", 4, 8, WideKind::Extend { off: 0 }),
    wide("i64x2_extend_high_i32x4_u", "u32", "u64", 4, 8, WideKind::Extend { off: 8 }),
    wide("i8x16_narrow_i16x8_s", "i16", "i8",  2, 1, WideKind::Narrow { min: "i8::MIN as i16",  max: "i8::MAX as i16" }),
    wide("i8x16_narrow_i16x8_u", "i16", "u8",  2, 1, WideKind::Narrow { min: "0",               max: "u8::MAX as i16" }),
    wide("i16x8_narrow_i32x4_s", "i32", "i16", 4, 2, WideKind::Narrow { min: "i16::MIN as i32", max: "i16::MAX as i32" }),
    wide("i16x8_narrow_i32x4_u", "i32", "u16", 4, 2, WideKind::Narrow { min: "0",               max: "u16::MAX as i32" }),
    wide("i16x8_extmul_low_i8x16_s",  "i8",  "i16", 1, 2, WideKind::ExtMul { off: 0 }),
    wide("i16x8_extmul_high_i8x16_s", "i8",  "i16", 1, 2, WideKind::ExtMul { off: 8 }),
    wide("i16x8_extmul_low_i8x16_u",  "u8",  "u16", 1, 2, WideKind::ExtMul { off: 0 }),
    wide("i16x8_extmul_high_i8x16_u", "u8",  "u16", 1, 2, WideKind::ExtMul { off: 8 }),
    wide("i32x4_extmul_low_i16x8_s",  "i16", "i32", 2, 4, WideKind::ExtMul { off: 0 }),
    wide("i32x4_extmul_high_i16x8_s", "i16", "i32", 2, 4, WideKind::ExtMul { off: 8 }),
    wide("i32x4_extmul_low_i16x8_u",  "u16", "u32", 2, 4, WideKind::ExtMul { off: 0 }),
    wide("i32x4_extmul_high_i16x8_u", "u16", "u32", 2, 4, WideKind::ExtMul { off: 8 }),
    wide("i64x2_extmul_low_i32x4_s",  "i32", "i64", 4, 8, WideKind::ExtMul { off: 0 }),
    wide("i64x2_extmul_high_i32x4_s", "i32", "i64", 4, 8, WideKind::ExtMul { off: 8 }),
    wide("i64x2_extmul_low_i32x4_u",  "u32", "u64", 4, 8, WideKind::ExtMul { off: 0 }),
    wide("i64x2_extmul_high_i32x4_u", "u32", "u64", 4, 8, WideKind::ExtMul { off: 8 }),
    wide("i16x8_extadd_pairwise_i8x16_s", "i8",  "i16", 1, 2, WideKind::ExtAddPairwise),
    wide("i16x8_extadd_pairwise_i8x16_u", "u8",  "u16", 1, 2, WideKind::ExtAddPairwise),
    wide("i32x4_extadd_pairwise_i16x8_s", "i16", "i32", 2, 4, WideKind::ExtAddPairwise),
    wide("i32x4_extadd_pairwise_i16x8_u", "u16", "u32", 2, 4, WideKind::ExtAddPairwise),
    wide("i32x4_dot_i16x8_s", "i16", "i32", 2, 4, WideKind::Dot),
    // Float <-> integer lane conversions. Rust's `as` cast already saturates
    // float-to-int, so `trunc_sat` needs no extra clamp. Those whose output is
    // filled from the source's low bytes (4->4, or low-half 4->8) reuse
    // `Extend { off: 0 }`; the `_zero` variants (8->4, two lanes to the low half,
    // upper lanes zeroed) use `NarrowZero`.
    wide("i32x4_trunc_sat_f32x4_s", "f32", "i32", 4, 4, WideKind::Extend { off: 0 }),
    wide("i32x4_trunc_sat_f32x4_u", "f32", "u32", 4, 4, WideKind::Extend { off: 0 }),
    wide("f32x4_convert_i32x4_s",   "i32", "f32", 4, 4, WideKind::Extend { off: 0 }),
    wide("f32x4_convert_i32x4_u",   "u32", "f32", 4, 4, WideKind::Extend { off: 0 }),
    wide("f64x2_convert_low_i32x4_s", "i32", "f64", 4, 8, WideKind::Extend { off: 0 }),
    wide("f64x2_convert_low_i32x4_u", "u32", "f64", 4, 8, WideKind::Extend { off: 0 }),
    wide("f64x2_promote_low_f32x4",   "f32", "f64", 4, 8, WideKind::Extend { off: 0 }),
    wide("i32x4_trunc_sat_f64x2_s_zero", "f64", "i32", 8, 4, WideKind::NarrowZero),
    wide("i32x4_trunc_sat_f64x2_u_zero", "f64", "u32", 8, 4, WideKind::NarrowZero),
    wide("f32x4_demote_f64x2_zero",      "f64", "f32", 8, 4, WideKind::NarrowZero),
];

/// A lane-reducing helper `fn(a: u128) -> i32` that collapses a vector to a
/// scalar. `elem` is the signed lane type (so `bitmask` can test the sign bit
/// with `< 0`) and `bytes` its width.
struct Reduce {
    name: &'static str,
    elem: &'static str,
    bytes: u32,
    kind: ReduceKind,
}

/// The two lane-reducing shapes.
enum ReduceKind {
    /// `all_true`: 1 if every lane is non-zero, else 0.
    AllTrue,
    /// `bitmask`: gather each lane's sign bit into bit `lane index` of an i32.
    Bitmask,
}

const fn reduce(name: &'static str, elem: &'static str, bytes: u32, kind: ReduceKind) -> Reduce {
    Reduce {
        name,
        elem,
        bytes,
        kind,
    }
}

#[rustfmt::skip]
const REDUCE: &[Reduce] = &[
    reduce("i8x16_all_true", "i8",  1, ReduceKind::AllTrue),
    reduce("i16x8_all_true", "i16", 2, ReduceKind::AllTrue),
    reduce("i32x4_all_true", "i32", 4, ReduceKind::AllTrue),
    reduce("i64x2_all_true", "i64", 8, ReduceKind::AllTrue),
    reduce("i8x16_bitmask",  "i8",  1, ReduceKind::Bitmask),
    reduce("i16x8_bitmask",  "i16", 2, ReduceKind::Bitmask),
    reduce("i32x4_bitmask",  "i32", 4, ReduceKind::Bitmask),
    reduce("i64x2_bitmask",  "i64", 8, ReduceKind::Bitmask),
];

/// Render the used lane helpers as module-scope free functions, in [`LANE`],
/// [`WIDE`], then [`REDUCE`] order, separated by blank lines. Empty string if
/// none are used.
pub(super) fn render_simd_helpers(used: &HashSet<&'static str>) -> String {
    let mut blocks: Vec<String> = Vec::new();
    for lane in LANE {
        if used.contains(lane.name) {
            blocks.push(lane_lines(lane).join("\n"));
        }
    }
    for w in WIDE {
        if used.contains(w.name) {
            blocks.push(wide_lines(w).join("\n"));
        }
    }
    for r in REDUCE {
        if used.contains(r.name) {
            blocks.push(reduce_lines(r).join("\n"));
        }
    }
    let mut out = blocks.join("\n\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

/// The unsigned integer type holding a full-width lane mask of `bytes` bytes.
fn umask(bytes: u32) -> &'static str {
    match bytes {
        1 => "u8",
        2 => "u16",
        4 => "u32",
        _ => "u64",
    }
}

/// The per-lane result expression in terms of the bound lane values `x` (and
/// `y` for binary lanes, `k` for shifts).
fn combine_expr(lane: &Lane) -> String {
    match lane.combine {
        Combine::Infix(op) => format!("x {op} y"),
        Combine::Method(m) => {
            if matches!(lane.shape, Shape::Binary) {
                format!("x.{m}(y)")
            } else {
                format!("x.{m}()")
            }
        }
        Combine::MinMax { equal, order } => format!(
            "if x.is_nan() || y.is_nan() {{ {elem}::NAN }} \
             else if x == y {{ {equal} }} else if x {order} y {{ x }} else {{ y }}",
            elem = lane.elem,
        ),
        Combine::Expr(e) => e.to_string(),
        Combine::Avgr => format!("(((x as u32) + (y as u32) + 1) >> 1) as {}", lane.elem),
        Combine::Compare(op) => {
            format!("if x {op} y {{ {}::MAX }} else {{ 0 }}", umask(lane.bytes))
        }
        Combine::Shift(op) => format!("x {op} k"),
    }
}

/// The source lines of one lane helper: read each lane as `elem`, combine, and
/// write the result back, iterating over the 16 bytes in `bytes`-wide steps.
fn lane_lines(lane: &Lane) -> Vec<String> {
    let &Lane {
        name,
        elem,
        bytes,
        shape,
        ..
    } = lane;
    let sig = match shape {
        Shape::Unary => "a: u128",
        Shape::Binary => "a: u128, b: u128",
        Shape::Shift => "a: u128, s: i32",
    };
    let mut lines = vec![
        format!("fn {name}({sig}) -> u128 {{"),
        "    let a = a.to_le_bytes();".to_string(),
    ];
    match shape {
        Shape::Binary => lines.push("    let b = b.to_le_bytes();".to_string()),
        Shape::Shift => lines.push(format!("    let k = (s as u32) % {};", bytes * 8)),
        Shape::Unary => {}
    }
    lines.push("    let mut r = [0u8; 16];".to_string());
    lines.push("    let mut i = 0;".to_string());
    lines.push("    while i < 16 {".to_string());
    lines.push(format!(
        "        let x = {elem}::from_le_bytes(a[i..i + {bytes}].try_into().unwrap());"
    ));
    if matches!(shape, Shape::Binary) {
        lines.push(format!(
            "        let y = {elem}::from_le_bytes(b[i..i + {bytes}].try_into().unwrap());"
        ));
    }
    lines.push(format!(
        "        r[i..i + {bytes}].copy_from_slice(&({}).to_le_bytes());",
        combine_expr(lane)
    ));
    lines.push(format!("        i += {bytes};"));
    lines.push("    }".to_string());
    lines.push("    u128::from_le_bytes(r)".to_string());
    lines.push("}".to_string());
    lines
}

/// The source lines of one width-changing helper. All but `Narrow` are output
/// driven (iterate output lanes, widen source lane(s), write). `Extend` casts
/// each source lane at offset `off` up to `out_elem`. `ExtMul` reads a matching
/// lane from both vectors at `off` and multiplies them widened. `ExtAddPairwise`
/// widens and sums each adjacent lane pair of one vector. `Dot` sums the two
/// adjacent widened products of both vectors, wrapping on overflow. `Narrow` is
/// input driven: it walks both inputs in `in_bytes` steps, saturating each lane
/// into `[min, max]` before casting down, writing the first vector's results to
/// the low 8 bytes and the second's to the high 8. `NarrowZero` is input driven
/// over one vector, casting each lane down (the `as` cast saturates float-to-int)
/// into the low lanes and leaving the upper lanes zero.
fn wide_lines(w: &Wide) -> Vec<String> {
    let &Wide {
        name,
        in_elem,
        out_elem,
        in_bytes,
        out_bytes,
        ..
    } = w;
    // The pairwise kinds (`ExtAddPairwise`, `Dot`) advance two source lanes per
    // output lane.
    let two = 2 * in_bytes;
    match w.kind {
        WideKind::Extend { off } => vec![
            format!("fn {name}(a: u128) -> u128 {{"),
            "    let a = a.to_le_bytes();".to_string(),
            "    let mut r = [0u8; 16];".to_string(),
            "    let mut o = 0;".to_string(),
            format!("    let mut s = {off};"),
            "    while o < 16 {".to_string(),
            format!(
                "        let x = {in_elem}::from_le_bytes(a[s..s + {in_bytes}].try_into().unwrap());"
            ),
            format!(
                "        r[o..o + {out_bytes}].copy_from_slice(&(x as {out_elem}).to_le_bytes());"
            ),
            format!("        o += {out_bytes};"),
            format!("        s += {in_bytes};"),
            "    }".to_string(),
            "    u128::from_le_bytes(r)".to_string(),
            "}".to_string(),
        ],
        WideKind::Narrow { min, max } => vec![
            format!("fn {name}(a: u128, b: u128) -> u128 {{"),
            "    let a = a.to_le_bytes();".to_string(),
            "    let b = b.to_le_bytes();".to_string(),
            "    let mut r = [0u8; 16];".to_string(),
            "    let mut o = 0;".to_string(),
            "    let mut s = 0;".to_string(),
            "    while s < 16 {".to_string(),
            format!(
                "        let x = {in_elem}::from_le_bytes(a[s..s + {in_bytes}].try_into().unwrap());"
            ),
            format!(
                "        r[o..o + {out_bytes}]\
                 .copy_from_slice(&(x.clamp({min}, {max}) as {out_elem}).to_le_bytes());"
            ),
            format!(
                "        let y = {in_elem}::from_le_bytes(b[s..s + {in_bytes}].try_into().unwrap());"
            ),
            format!(
                "        r[o + 8..o + 8 + {out_bytes}]\
                 .copy_from_slice(&(y.clamp({min}, {max}) as {out_elem}).to_le_bytes());"
            ),
            format!("        o += {out_bytes};"),
            format!("        s += {in_bytes};"),
            "    }".to_string(),
            "    u128::from_le_bytes(r)".to_string(),
            "}".to_string(),
        ],
        WideKind::NarrowZero => vec![
            format!("fn {name}(a: u128) -> u128 {{"),
            "    let a = a.to_le_bytes();".to_string(),
            "    let mut r = [0u8; 16];".to_string(),
            "    let mut o = 0;".to_string(),
            "    let mut s = 0;".to_string(),
            "    while s < 16 {".to_string(),
            format!(
                "        let x = {in_elem}::from_le_bytes(a[s..s + {in_bytes}].try_into().unwrap());"
            ),
            format!(
                "        r[o..o + {out_bytes}].copy_from_slice(&(x as {out_elem}).to_le_bytes());"
            ),
            format!("        o += {out_bytes};"),
            format!("        s += {in_bytes};"),
            "    }".to_string(),
            "    u128::from_le_bytes(r)".to_string(),
            "}".to_string(),
        ],
        WideKind::ExtMul { off } => vec![
            format!("fn {name}(a: u128, b: u128) -> u128 {{"),
            "    let a = a.to_le_bytes();".to_string(),
            "    let b = b.to_le_bytes();".to_string(),
            "    let mut r = [0u8; 16];".to_string(),
            "    let mut o = 0;".to_string(),
            format!("    let mut s = {off};"),
            "    while o < 16 {".to_string(),
            format!(
                "        let x = {in_elem}::from_le_bytes(a[s..s + {in_bytes}].try_into().unwrap());"
            ),
            format!(
                "        let y = {in_elem}::from_le_bytes(b[s..s + {in_bytes}].try_into().unwrap());"
            ),
            format!(
                "        r[o..o + {out_bytes}]\
                 .copy_from_slice(&((x as {out_elem}) * (y as {out_elem})).to_le_bytes());"
            ),
            format!("        o += {out_bytes};"),
            format!("        s += {in_bytes};"),
            "    }".to_string(),
            "    u128::from_le_bytes(r)".to_string(),
            "}".to_string(),
        ],
        WideKind::ExtAddPairwise => vec![
            format!("fn {name}(a: u128) -> u128 {{"),
            "    let a = a.to_le_bytes();".to_string(),
            "    let mut r = [0u8; 16];".to_string(),
            "    let mut o = 0;".to_string(),
            "    let mut s = 0;".to_string(),
            "    while o < 16 {".to_string(),
            format!(
                "        let x = {in_elem}::from_le_bytes(a[s..s + {in_bytes}].try_into().unwrap());"
            ),
            format!(
                "        let y = {in_elem}\
                 ::from_le_bytes(a[s + {in_bytes}..s + {two}].try_into().unwrap());"
            ),
            format!(
                "        r[o..o + {out_bytes}]\
                 .copy_from_slice(&((x as {out_elem}) + (y as {out_elem})).to_le_bytes());"
            ),
            format!("        o += {out_bytes};"),
            format!("        s += {two};"),
            "    }".to_string(),
            "    u128::from_le_bytes(r)".to_string(),
            "}".to_string(),
        ],
        WideKind::Dot => vec![
            format!("fn {name}(a: u128, b: u128) -> u128 {{"),
            "    let a = a.to_le_bytes();".to_string(),
            "    let b = b.to_le_bytes();".to_string(),
            "    let mut r = [0u8; 16];".to_string(),
            "    let mut o = 0;".to_string(),
            "    let mut s = 0;".to_string(),
            "    while o < 16 {".to_string(),
            format!(
                "        let a0 = {in_elem}\
                 ::from_le_bytes(a[s..s + {in_bytes}].try_into().unwrap()) as {out_elem};"
            ),
            format!(
                "        let b0 = {in_elem}\
                 ::from_le_bytes(b[s..s + {in_bytes}].try_into().unwrap()) as {out_elem};"
            ),
            format!(
                "        let a1 = {in_elem}\
                 ::from_le_bytes(a[s + {in_bytes}..s + {two}].try_into().unwrap()) as {out_elem};"
            ),
            format!(
                "        let b1 = {in_elem}\
                 ::from_le_bytes(b[s + {in_bytes}..s + {two}].try_into().unwrap()) as {out_elem};"
            ),
            format!(
                "        r[o..o + {out_bytes}]\
                 .copy_from_slice(&(a0 * b0).wrapping_add(a1 * b1).to_le_bytes());"
            ),
            format!("        o += {out_bytes};"),
            format!("        s += {two};"),
            "    }".to_string(),
            "    u128::from_le_bytes(r)".to_string(),
            "}".to_string(),
        ],
    }
}

/// The source lines of one lane-reducing helper. Both read each lane as the
/// signed `elem` in `bytes`-wide steps. `AllTrue` returns 0 as soon as a lane is
/// zero, 1 otherwise. `Bitmask` sets bit `n` of the result from lane `n`'s sign.
fn reduce_lines(r: &Reduce) -> Vec<String> {
    let &Reduce {
        name, elem, bytes, ..
    } = r;
    let read = format!("{elem}::from_le_bytes(a[i..i + {bytes}].try_into().unwrap())");
    match r.kind {
        ReduceKind::AllTrue => vec![
            format!("fn {name}(a: u128) -> i32 {{"),
            "    let a = a.to_le_bytes();".to_string(),
            "    let mut i = 0;".to_string(),
            "    while i < 16 {".to_string(),
            format!("        if {read} == 0 {{"),
            "            return 0;".to_string(),
            "        }".to_string(),
            format!("        i += {bytes};"),
            "    }".to_string(),
            "    1".to_string(),
            "}".to_string(),
        ],
        ReduceKind::Bitmask => vec![
            format!("fn {name}(a: u128) -> i32 {{"),
            "    let a = a.to_le_bytes();".to_string(),
            "    let mut m = 0i32;".to_string(),
            "    let mut i = 0;".to_string(),
            "    while i < 16 {".to_string(),
            format!("        if {read} < 0 {{"),
            format!("            m |= 1 << (i / {bytes});"),
            "        }".to_string(),
            format!("        i += {bytes};"),
            "    }".to_string(),
            "    m".to_string(),
            "}".to_string(),
        ],
    }
}
