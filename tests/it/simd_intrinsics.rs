//! `simd_intrinsics` swaps the scalar per-lane loop of the integer SIMD helpers for
//! `core::arch` (NEON / SSE2) bodies. This is a differential test: one wasm module
//! applies every mapped op to the same edge-heavy inputs and folds every result lane
//! into a hash, and the default build and the intrinsic build must print the same
//! hash. Run on aarch64 and x86_64 CI alike, each against its own scalar fallback,
//! so a divergence on either architecture (saturation, signed/unsigned compare via
//! bias, `avgr`) shows up.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    reason = "test code"
)]

use std::fmt::Write as _;
use std::process::Command;

use wasm2rs::TranspileOptions;

use crate::common::build;

const RANDOM_PAIRS: usize = 300;
/// Fixed vectors: every lane width's MIN / MAX / -1 / 0, which the random bytes
/// almost never produce as a whole lane (`abs`/`neg` of MIN, saturation limits).
const FIXED: [[u8; 16]; 8] = [
    [0x80; 16],
    [0x7f; 16],
    [0xff; 16],
    [0x00; 16],
    [
        0x00, 0x80, 0x00, 0x80, 0x00, 0x80, 0x00, 0x80, 0x00, 0x80, 0x00, 0x80, 0x00, 0x80, 0x00,
        0x80,
    ],
    [
        0x00, 0x00, 0x00, 0x80, 0x00, 0x00, 0x00, 0x80, 0x00, 0x00, 0x00, 0x80, 0x00, 0x00, 0x00,
        0x80,
    ],
    [
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x80,
    ],
    [
        0xff, 0x7f, 0xff, 0x7f, 0xff, 0xff, 0xff, 0x7f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0x7f,
    ],
];
const PAIRS: usize = RANDOM_PAIRS + FIXED.len() * FIXED.len();

/// Lane-wise binary ops (`a`, `b` -> v128) the intrinsic build maps.
fn binary_ops() -> Vec<String> {
    let mut ops = Vec::new();
    for sh in ["i8x16", "i16x8", "i32x4", "i64x2"] {
        for op in ["add", "sub"] {
            ops.push(format!("{sh}.{op}"));
        }
    }
    for sh in ["i16x8", "i32x4", "i64x2"] {
        ops.push(format!("{sh}.mul"));
    }
    for sh in ["i8x16", "i16x8"] {
        for op in ["add_sat_s", "add_sat_u", "sub_sat_s", "sub_sat_u"] {
            ops.push(format!("{sh}.{op}"));
        }
        ops.push(format!("{sh}.avgr_u"));
    }
    for sh in ["i8x16", "i16x8", "i32x4"] {
        for op in [
            "eq", "ne", "lt_s", "lt_u", "gt_s", "gt_u", "le_s", "le_u", "ge_s", "ge_u",
        ] {
            ops.push(format!("{sh}.{op}"));
        }
    }
    for op in ["eq", "ne", "lt_s", "gt_s", "le_s", "ge_s"] {
        ops.push(format!("i64x2.{op}"));
    }
    ops
}

/// Lane-wise unary ops the intrinsic build maps.
fn unary_ops() -> Vec<String> {
    let mut ops = Vec::new();
    for sh in ["i8x16", "i16x8", "i32x4", "i64x2"] {
        ops.push(format!("{sh}.neg"));
        ops.push(format!("{sh}.abs"));
    }
    ops
}

/// A fixed-seed generator: the test must be reproducible.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// Half the bytes are random, half come from the values where lane
    /// arithmetic wraps, saturates or flips sign.
    fn bytes(&mut self) -> [u8; 16] {
        const EDGE: [u8; 8] = [0x00, 0x01, 0x7f, 0x80, 0x81, 0xfe, 0xff, 0x40];
        let mut out = [0u8; 16];
        for b in &mut out {
            let r = self.next();
            *b = if r & 1 == 0 {
                (r >> 8) as u8
            } else {
                EDGE[(r >> 8) as usize % EDGE.len()]
            };
        }
        out
    }
}

fn data_segment() -> String {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut s = String::new();
    for (a, b) in FIXED
        .iter()
        .flat_map(|a| FIXED.iter().map(move |b| (*a, *b)))
    {
        for byte in a.into_iter().chain(b) {
            write!(s, "\\{byte:02x}").unwrap();
        }
    }
    for i in 0..RANDOM_PAIRS {
        let a = rng.bytes();
        // Every fifth pair is equal and every seventh differs in one byte, so
        // `eq`/`ne`/`le`/`ge` see their boundary cases.
        let b = if i % 5 == 0 {
            a
        } else if i % 7 == 0 {
            let mut b = a;
            b[i % 16] ^= 1;
            b
        } else {
            rng.bytes()
        };
        for byte in a.into_iter().chain(b) {
            write!(s, "\\{byte:02x}").unwrap();
        }
    }
    s
}

/// `v128 -> i64` fold: both lanes, so no result bit goes unchecked.
const FOLD: &str = "(i64.add (i64x2.extract_lane 0 (local.get $r)) \
                    (i64.mul (i64x2.extract_lane 1 (local.get $r)) (i64.const 0x9E3779B97F4A7C15)))";

fn module_wat() -> String {
    let mut body = String::new();
    let mut step = |apply: String| {
        write!(
            body,
            "(local.set $r {apply})
             (local.set $acc (i64.add (i64.mul (local.get $acc) (i64.const 1099511628211)) {FOLD}))\n"
        )
        .unwrap();
    };
    for op in binary_ops() {
        step(format!("({op} (local.get $a) (local.get $b))"));
    }
    for op in unary_ops() {
        step(format!("({op} (local.get $a))"));
    }
    format!(
        r#"(module
          (memory 1)
          (data (i32.const 0) "{data}")
          (func (param i32) (result i64)
            (local $i i32) (local $acc i64) (local $a v128) (local $b v128) (local $r v128)
            (block $done
              (loop $next
                (br_if $done (i32.ge_u (local.get $i) (local.get 0)))
                (local.set $a (v128.load (i32.mul (local.get $i) (i32.const 32))))
                (local.set $b (v128.load offset=16 (i32.mul (local.get $i) (i32.const 32))))
                {body}
                (local.set $i (i32.add (local.get $i) (i32.const 1)))
                (br $next)))
            (local.get $acc)))"#,
        data = data_segment(),
    )
}

fn run(name: &str, wasm: &[u8], opts: &TranspileOptions) -> (String, String) {
    let generated = wasm2rs::transpile_with_options(wasm, opts).expect("transpile ok");
    let program = format!(
        "{generated}\nfn main() {{\n    let mut inst = Instance::new();\n    \
         println!(\"{{}}\", inst.func0({PAIRS}));\n}}\n"
    );
    let bin = build(name, &program);
    let out = Command::new(bin).output().expect("run generated binary");
    assert!(out.status.success(), "{name} did not run");
    (String::from_utf8(out.stdout).unwrap(), generated)
}

#[test]
fn intrinsic_lane_ops_match_the_scalar_loop() {
    let wasm = wat::parse_str(module_wat()).expect("valid wat");
    // The two rustc builds dominate the runtime, so overlap them.
    let ((scalar, scalar_src), (arch, arch_src)) = std::thread::scope(|s| {
        let off = s.spawn(|| run("simd_intrinsics_off", &wasm, &TranspileOptions::default()));
        let on = s.spawn(|| {
            run(
                "simd_intrinsics_on",
                &wasm,
                &TranspileOptions::default().with_simd_intrinsics(true),
            )
        });
        (off.join().unwrap(), on.join().unwrap())
    });
    assert!(!scalar_src.contains("core::arch"), "off by default");
    assert!(!scalar_src.contains("unsafe"), "default output stays safe");
    assert!(
        arch_src.contains("core::arch"),
        "the option emits intrinsics"
    );
    assert_eq!(scalar, arch, "intrinsic and scalar lane ops must agree");
}
