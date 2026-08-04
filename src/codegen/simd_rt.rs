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

/// Render the used lane helpers as module-scope free functions, in [`LANE`]
/// order, separated by blank lines. Returns an empty string if none are used.
pub(super) fn render_simd_helpers(used: &HashSet<&'static str>) -> String {
    let mut blocks: Vec<String> = Vec::new();
    for lane in LANE {
        if used.contains(lane.name) {
            blocks.push(lane_lines(lane).join("\n"));
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
