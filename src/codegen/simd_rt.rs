//! Lane-wise SIMD runtime helpers: module-scope free functions over `u128`
//! (a v128), each splitting the register into lanes, applying a per-lane scalar
//! operation, and repacking. Unlike the scalar [`super::Rt`] helpers there are
//! many and they are highly regular, so they are described by a table ([`LANE`])
//! and generated from a single template rather than written out by hand. A
//! function tracks the ones it uses by name (`used_simd`), matching how it
//! tracks [`super::Helper`]/[`super::Rt`] dependencies.

use std::collections::HashSet;

/// How a lane helper combines the bound lane value(s) into the per-lane result,
/// written in terms of `x` (and `y` for binary ops).
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
}

/// For equal (±0) operands, `min` keeps the negatively-signed lane, `max` the
/// positively-signed one.
const MIN_EQUAL: &str = "if x.is_sign_negative() { x } else { y }";
const MAX_EQUAL: &str = "if x.is_sign_negative() { y } else { x }";

/// One lane helper: `name` is the emitted function name, `elem` the Rust lane
/// type, `bytes` its width, `binary` whether it takes two vectors (`fn(a, b)`)
/// or one (`fn(a)`), and `combine` how the lane result is formed. Integer lanes
/// use a signed `elem` because wrapping add/sub/mul/neg produce identical bits
/// for signed and unsigned lanes.
struct Lane {
    name: &'static str,
    elem: &'static str,
    bytes: u32,
    binary: bool,
    combine: Combine,
}

const fn lane(
    name: &'static str,
    elem: &'static str,
    bytes: u32,
    binary: bool,
    combine: Combine,
) -> Lane {
    Lane {
        name,
        elem,
        bytes,
        binary,
        combine,
    }
}

/// All lane helpers, in a deterministic emission order. `#[rustfmt::skip]` keeps
/// each entry on one line so the table reads as a table (rustfmt would otherwise
/// explode the longer entries across five lines each).
#[rustfmt::skip]
const LANE: &[Lane] = &[
    // Integer wrapping arithmetic.
    lane("i8x16_add", "i8", 1, true, Combine::Method("wrapping_add")),
    lane("i16x8_add", "i16", 2, true, Combine::Method("wrapping_add")),
    lane("i32x4_add", "i32", 4, true, Combine::Method("wrapping_add")),
    lane("i64x2_add", "i64", 8, true, Combine::Method("wrapping_add")),
    lane("i8x16_sub", "i8", 1, true, Combine::Method("wrapping_sub")),
    lane("i16x8_sub", "i16", 2, true, Combine::Method("wrapping_sub")),
    lane("i32x4_sub", "i32", 4, true, Combine::Method("wrapping_sub")),
    lane("i64x2_sub", "i64", 8, true, Combine::Method("wrapping_sub")),
    lane("i16x8_mul", "i16", 2, true, Combine::Method("wrapping_mul")),
    lane("i32x4_mul", "i32", 4, true, Combine::Method("wrapping_mul")),
    lane("i64x2_mul", "i64", 8, true, Combine::Method("wrapping_mul")),
    lane("i8x16_neg", "i8", 1, false, Combine::Method("wrapping_neg")),
    lane("i16x8_neg", "i16", 2, false, Combine::Method("wrapping_neg")),
    lane("i32x4_neg", "i32", 4, false, Combine::Method("wrapping_neg")),
    lane("i64x2_neg", "i64", 8, false, Combine::Method("wrapping_neg")),
    // Float arithmetic.
    lane("f32x4_add", "f32", 4, true, Combine::Infix("+")),
    lane("f64x2_add", "f64", 8, true, Combine::Infix("+")),
    lane("f32x4_sub", "f32", 4, true, Combine::Infix("-")),
    lane("f64x2_sub", "f64", 8, true, Combine::Infix("-")),
    lane("f32x4_mul", "f32", 4, true, Combine::Infix("*")),
    lane("f64x2_mul", "f64", 8, true, Combine::Infix("*")),
    lane("f32x4_div", "f32", 4, true, Combine::Infix("/")),
    lane("f64x2_div", "f64", 8, true, Combine::Infix("/")),
    lane("f32x4_min", "f32", 4, true, Combine::MinMax { equal: MIN_EQUAL, order: "<" }),
    lane("f64x2_min", "f64", 8, true, Combine::MinMax { equal: MIN_EQUAL, order: "<" }),
    lane("f32x4_max", "f32", 4, true, Combine::MinMax { equal: MAX_EQUAL, order: ">" }),
    lane("f64x2_max", "f64", 8, true, Combine::MinMax { equal: MAX_EQUAL, order: ">" }),
    lane("f32x4_pmin", "f32", 4, true, Combine::Expr("if y < x { y } else { x }")),
    lane("f64x2_pmin", "f64", 8, true, Combine::Expr("if y < x { y } else { x }")),
    lane("f32x4_pmax", "f32", 4, true, Combine::Expr("if x < y { y } else { x }")),
    lane("f64x2_pmax", "f64", 8, true, Combine::Expr("if x < y { y } else { x }")),
    // Float unary (per-lane method).
    lane("f32x4_sqrt", "f32", 4, false, Combine::Method("sqrt")),
    lane("f64x2_sqrt", "f64", 8, false, Combine::Method("sqrt")),
    lane("f32x4_ceil", "f32", 4, false, Combine::Method("ceil")),
    lane("f64x2_ceil", "f64", 8, false, Combine::Method("ceil")),
    lane("f32x4_floor", "f32", 4, false, Combine::Method("floor")),
    lane("f64x2_floor", "f64", 8, false, Combine::Method("floor")),
    lane("f32x4_trunc", "f32", 4, false, Combine::Method("trunc")),
    lane("f64x2_trunc", "f64", 8, false, Combine::Method("trunc")),
    lane("f32x4_nearest", "f32", 4, false, Combine::Method("round_ties_even")),
    lane("f64x2_nearest", "f64", 8, false, Combine::Method("round_ties_even")),
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

/// The per-lane result expression in terms of the bound lane values `x` (and
/// `y` for binary lanes).
fn combine_expr(lane: &Lane) -> String {
    match lane.combine {
        Combine::Infix(op) => format!("x {op} y"),
        Combine::Method(m) => {
            if lane.binary {
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
    }
}

/// The source lines of one lane helper: read each lane as `elem`, combine, and
/// write the result back, iterating over the 16 bytes in `bytes`-wide steps.
fn lane_lines(lane: &Lane) -> Vec<String> {
    let &Lane {
        name,
        elem,
        bytes,
        binary,
        ..
    } = lane;
    let sig = if binary {
        "a: u128, b: u128"
    } else {
        "a: u128"
    };
    let mut lines = vec![
        format!("fn {name}({sig}) -> u128 {{"),
        "    let a = a.to_le_bytes();".to_string(),
    ];
    if binary {
        lines.push("    let b = b.to_le_bytes();".to_string());
    }
    lines.push("    let mut r = [0u8; 16];".to_string());
    lines.push("    let mut i = 0;".to_string());
    lines.push("    while i < 16 {".to_string());
    lines.push(format!(
        "        let x = {elem}::from_le_bytes(a[i..i + {bytes}].try_into().unwrap());"
    ));
    if binary {
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
