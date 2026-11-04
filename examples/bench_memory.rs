//! Measures what bounds checks cost on memory-heavy workloads: each workload in
//! `benches/memory/` is transpiled with and without `unsafe_memory` (the latter
//! drops every check, so it is the upper bound on what removing checks could
//! gain), built with `rustc -O`, and timed in-process.
//!
//! Usage: `cargo run --release --example bench_memory [runs]` (default 7 runs,
//! the median is reported). Prints a Markdown table.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use wasm2rs::{TranspileOptions, transpile_with_options};

/// (name, wat, `run` argument sized for roughly a second of work).
const WORKLOADS: [(&str, &str, u32); 3] = [
    ("hash", include_str!("../benches/memory/hash.wat"), 15_000),
    (
        "matmul",
        include_str!("../benches/memory/matmul.wat"),
        12_000,
    ),
    (
        "random",
        include_str!("../benches/memory/random.wat"),
        800_000_000,
    ),
];

fn build(dir: &Path, name: &str, wat: &str, n: u32, unsafe_memory: bool) -> std::path::PathBuf {
    let wasm = wat::parse_str(wat).expect("valid wat");
    let opts = TranspileOptions {
        named_exports: true,
        unsafe_memory,
        ..TranspileOptions::default()
    };
    let generated = transpile_with_options(&wasm, &opts).expect("transpile");
    // `drive` is never inlined and the instance is opaque to the optimizer: with
    // the memory's length a visible constant, LLVM proves every access in bounds
    // and the safe and unsafe builds compile to identical code.
    let program = format!(
        "{generated}\n#[inline(never)]\nfn drive(m: &mut Instance, n: i32) -> i32 {{ m.run(n) }}\n\
         fn main() {{\n\
         \x20   let mut m = std::hint::black_box(Instance::new());\n\
         \x20   let t = std::time::Instant::now();\n\
         \x20   let r = std::hint::black_box(drive(&mut m, std::hint::black_box({n})));\n\
         \x20   println!(\"{{}} {{}}\", t.elapsed().as_nanos(), r);\n\
         }}\n"
    );
    let mode = if unsafe_memory { "unsafe" } else { "safe" };
    let src = dir.join(format!("{name}_{mode}.rs"));
    let bin = dir.join(format!("{name}_{mode}"));
    std::fs::write(&src, program).expect("write source");
    let status = Command::new("rustc")
        .args(["--edition", "2024", "-C", "opt-level=3", "-A", "warnings"])
        .arg(&src)
        .arg("-o")
        .arg(&bin)
        .status()
        .expect("run rustc");
    assert!(status.success(), "rustc failed for {name} {mode}");
    bin
}

/// Run `bin` once; returns (elapsed, checksum).
fn time(bin: &Path) -> (Duration, String) {
    let out = Command::new(bin).output().expect("run workload");
    assert!(out.status.success(), "workload failed");
    let text = String::from_utf8(out.stdout).expect("utf8");
    let (ns, sum) = text.trim().split_once(' ').expect("`ns checksum`");
    (
        Duration::from_nanos(ns.parse().expect("nanos")),
        sum.to_string(),
    )
}

fn median(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v[v.len() / 2]
}

fn main() {
    let runs: usize = std::env::args()
        .nth(1)
        .map_or(7, |a| a.parse().expect("runs"));
    let dir = std::env::temp_dir().join(format!("wasm2rs-bench-memory-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create dir");

    println!("| workload | safe (ms) | unsafe (ms) | safe / unsafe |");
    println!("|---|---|---|---|");
    for (name, wat, n) in WORKLOADS {
        let safe = build(&dir, name, wat, n, false);
        let fast = build(&dir, name, wat, n, true);
        let (mut safe_t, mut fast_t) = (Vec::new(), Vec::new());
        let mut sum = None;
        // Interleave the two so thermal / frequency drift hits both equally.
        for _ in 0..runs {
            let (t, s) = time(&safe);
            safe_t.push(t);
            let (t, f) = time(&fast);
            fast_t.push(t);
            assert_eq!(s, f, "{name}: checksums differ");
            sum = Some(s);
        }
        let (s, f) = (median(safe_t), median(fast_t));
        println!(
            "| {name} | {:.1} | {:.1} | {:.3} |  <!-- checksum {} -->",
            s.as_secs_f64() * 1e3,
            f.as_secs_f64() * 1e3,
            s.as_secs_f64() / f.as_secs_f64(),
            sum.unwrap_or_default()
        );
    }
    std::fs::remove_dir_all(&dir).ok();
}
