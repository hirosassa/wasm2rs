//! Lane-wise SIMD runtime helpers: module-scope free functions over `u128`
//! (a v128), each splitting the register into lanes, applying a per-lane scalar
//! operation, and repacking. Unlike the scalar [`super::Rt`] helpers there are
//! many and they are highly regular, so they are described by a table ([`LANE`])
//! and generated from a single template rather than written out by hand. A
//! function tracks the ones it uses by name (`used_simd`), matching how it
//! tracks [`super::Helper`]/[`super::Rt`] dependencies.

use std::collections::HashSet;

/// One lane helper: `name` is the emitted function name, `elem` the Rust lane
/// type, `bytes` its width, `method` the wrapping scalar method applied per
/// lane, and `binary` whether it takes two vectors (`fn(a, b)`) or one
/// (`fn(a)`). `elem` is always a signed integer: wrapping add/sub/mul/neg
/// produce identical bits for signed and unsigned lanes.
struct Lane {
    name: &'static str,
    elem: &'static str,
    bytes: u32,
    method: &'static str,
    binary: bool,
}

/// All lane helpers, in a deterministic emission order.
const LANE: &[Lane] = &[
    Lane {
        name: "i8x16_add",
        elem: "i8",
        bytes: 1,
        method: "wrapping_add",
        binary: true,
    },
    Lane {
        name: "i16x8_add",
        elem: "i16",
        bytes: 2,
        method: "wrapping_add",
        binary: true,
    },
    Lane {
        name: "i32x4_add",
        elem: "i32",
        bytes: 4,
        method: "wrapping_add",
        binary: true,
    },
    Lane {
        name: "i64x2_add",
        elem: "i64",
        bytes: 8,
        method: "wrapping_add",
        binary: true,
    },
    Lane {
        name: "i8x16_sub",
        elem: "i8",
        bytes: 1,
        method: "wrapping_sub",
        binary: true,
    },
    Lane {
        name: "i16x8_sub",
        elem: "i16",
        bytes: 2,
        method: "wrapping_sub",
        binary: true,
    },
    Lane {
        name: "i32x4_sub",
        elem: "i32",
        bytes: 4,
        method: "wrapping_sub",
        binary: true,
    },
    Lane {
        name: "i64x2_sub",
        elem: "i64",
        bytes: 8,
        method: "wrapping_sub",
        binary: true,
    },
    Lane {
        name: "i16x8_mul",
        elem: "i16",
        bytes: 2,
        method: "wrapping_mul",
        binary: true,
    },
    Lane {
        name: "i32x4_mul",
        elem: "i32",
        bytes: 4,
        method: "wrapping_mul",
        binary: true,
    },
    Lane {
        name: "i64x2_mul",
        elem: "i64",
        bytes: 8,
        method: "wrapping_mul",
        binary: true,
    },
    Lane {
        name: "i8x16_neg",
        elem: "i8",
        bytes: 1,
        method: "wrapping_neg",
        binary: false,
    },
    Lane {
        name: "i16x8_neg",
        elem: "i16",
        bytes: 2,
        method: "wrapping_neg",
        binary: false,
    },
    Lane {
        name: "i32x4_neg",
        elem: "i32",
        bytes: 4,
        method: "wrapping_neg",
        binary: false,
    },
    Lane {
        name: "i64x2_neg",
        elem: "i64",
        bytes: 8,
        method: "wrapping_neg",
        binary: false,
    },
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

/// The source lines of one lane helper: read each lane as `elem`, apply the
/// per-lane `method`, and write the result back, iterating over the 16 bytes in
/// `bytes`-wide steps.
fn lane_lines(lane: &Lane) -> Vec<String> {
    let &Lane {
        name,
        elem,
        bytes,
        method,
        binary,
    } = lane;
    let mut lines = Vec::new();
    let sig = if binary {
        "a: u128, b: u128"
    } else {
        "a: u128"
    };
    lines.push(format!("fn {name}({sig}) -> u128 {{"));
    lines.push("    let a = a.to_le_bytes();".to_string());
    if binary {
        lines.push("    let b = b.to_le_bytes();".to_string());
    }
    lines.push("    let mut r = [0u8; 16];".to_string());
    lines.push("    let mut i = 0;".to_string());
    lines.push("    while i < 16 {".to_string());
    lines.push(format!(
        "        let x = {elem}::from_le_bytes(a[i..i + {bytes}].try_into().unwrap());"
    ));
    let combined = if binary {
        lines.push(format!(
            "        let y = {elem}::from_le_bytes(b[i..i + {bytes}].try_into().unwrap());"
        ));
        format!("x.{method}(y)")
    } else {
        format!("x.{method}()")
    };
    lines.push(format!(
        "        r[i..i + {bytes}].copy_from_slice(&({combined}).to_le_bytes());"
    ));
    lines.push(format!("        i += {bytes};"));
    lines.push("    }".to_string());
    lines.push("    u128::from_le_bytes(r)".to_string());
    lines.push("}".to_string());
    lines
}
