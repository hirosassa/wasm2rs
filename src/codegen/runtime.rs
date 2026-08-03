use std::collections::HashSet;

use super::Rt;

pub(super) fn rt_name(rt: Rt) -> &'static str {
    match rt {
        Rt::F32Min => "f32_min",
        Rt::F32Max => "f32_max",
        Rt::F64Min => "f64_min",
        Rt::F64Max => "f64_max",
        Rt::I32TruncF32S => "i32_trunc_f32_s",
        Rt::I32TruncF32U => "i32_trunc_f32_u",
        Rt::I32TruncF64S => "i32_trunc_f64_s",
        Rt::I32TruncF64U => "i32_trunc_f64_u",
        Rt::I64TruncF32S => "i64_trunc_f32_s",
        Rt::I64TruncF32U => "i64_trunc_f32_u",
        Rt::I64TruncF64S => "i64_trunc_f64_s",
        Rt::I64TruncF64U => "i64_trunc_f64_u",
    }
}

/// All runtime free-function helpers, in a deterministic emission order.
const RT_ORDER: [Rt; 12] = [
    Rt::F32Min,
    Rt::F32Max,
    Rt::F64Min,
    Rt::F64Max,
    Rt::I32TruncF32S,
    Rt::I32TruncF32U,
    Rt::I32TruncF64S,
    Rt::I32TruncF64U,
    Rt::I64TruncF32S,
    Rt::I64TruncF32U,
    Rt::I64TruncF64S,
    Rt::I64TruncF64U,
];

/// Render the used runtime helpers as module-scope free functions, in
/// [`RT_ORDER`], separated by blank lines. Returns an empty string if none.
pub(super) fn render_rt_helpers(used: &HashSet<Rt>) -> String {
    let mut blocks: Vec<String> = Vec::new();
    for rt in RT_ORDER {
        if used.contains(&rt) {
            blocks.push(rt_lines(rt).join("\n"));
        }
    }
    let mut out = blocks.join("\n\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

/// The source lines of one runtime helper, dispatched to the min/max or the
/// trapping-truncation template.
fn rt_lines(rt: Rt) -> Vec<String> {
    match rt {
        Rt::F32Min | Rt::F32Max | Rt::F64Min | Rt::F64Max => rt_minmax_lines(rt),
        Rt::I32TruncF32S => {
            trunc_lines("i32_trunc_f32_s", "f32", "i32", TRUNC_I32_S_F32, "x as i32")
        }
        Rt::I32TruncF32U => trunc_lines(
            "i32_trunc_f32_u",
            "f32",
            "i32",
            TRUNC_U_F32_32,
            "x as u32 as i32",
        ),
        Rt::I32TruncF64S => {
            trunc_lines("i32_trunc_f64_s", "f64", "i32", TRUNC_I32_S_F64, "x as i32")
        }
        Rt::I32TruncF64U => trunc_lines(
            "i32_trunc_f64_u",
            "f64",
            "i32",
            TRUNC_U_F64_32,
            "x as u32 as i32",
        ),
        Rt::I64TruncF32S => {
            trunc_lines("i64_trunc_f32_s", "f32", "i64", TRUNC_I64_S_F32, "x as i64")
        }
        Rt::I64TruncF32U => trunc_lines(
            "i64_trunc_f32_u",
            "f32",
            "i64",
            TRUNC_U_F32_64,
            "x as u64 as i64",
        ),
        Rt::I64TruncF64S => {
            trunc_lines("i64_trunc_f64_s", "f64", "i64", TRUNC_I64_S_F64, "x as i64")
        }
        Rt::I64TruncF64U => trunc_lines(
            "i64_trunc_f64_u",
            "f64",
            "i64",
            TRUNC_U_F64_64,
            "x as u64 as i64",
        ),
    }
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
fn trunc_lines(name: &str, ft: &str, it: &str, range: &str, cast: &str) -> Vec<String> {
    vec![
        format!("fn {name}(x: {ft}) -> {it} {{"),
        "    if x.is_nan() {".to_string(),
        "        panic!(\"invalid conversion to integer\");".to_string(),
        "    }".to_string(),
        format!("    if !({range}) {{"),
        "        panic!(\"integer overflow\");".to_string(),
        "    }".to_string(),
        format!("    {cast}"),
        "}".to_string(),
    ]
}

/// wasm `min`/`max` return NaN if either operand is NaN, and when the operands
/// are equal (notably ±0) `min` yields the negatively-signed and `max` the
/// positively-signed value — differing from Rust's `f32::min`/`max`.
fn rt_minmax_lines(rt: Rt) -> Vec<String> {
    let owned = |lines: &[&str]| lines.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    let name = rt_name(rt);
    let ty = if matches!(rt, Rt::F32Min | Rt::F32Max) {
        "f32"
    } else {
        "f64"
    };
    // For equal operands, `min` keeps the negative one and `max` the positive
    // one; the `<`/`>` picks the smaller/larger otherwise.
    let (equal_pick, order_op) = if matches!(rt, Rt::F32Min | Rt::F64Min) {
        ("if a.is_sign_negative() { a } else { b }", "<")
    } else {
        ("if a.is_sign_negative() { b } else { a }", ">")
    };
    owned(&[
        &format!("fn {name}(a: {ty}, b: {ty}) -> {ty} {{"),
        "    if a.is_nan() || b.is_nan() {",
        &format!("        return {ty}::NAN;"),
        "    }",
        "    if a == b {",
        &format!("        {equal_pick}"),
        &format!("    }} else if a {order_op} b {{"),
        "        a",
        "    } else {",
        "        b",
        "    }",
        "}",
    ])
}
